use super::{
    DownstreamConfig, ModelContextConfig, ModelRequestCostConfig, PersistedState, UpstreamConfig,
    UpstreamProtocol, UsageLog,
};
use bb8::Pool;
use bb8_postgres::PostgresConnectionManager;
use std::collections::{HashMap, HashSet};
use std::env;
use std::io;
use std::str::FromStr;
use std::time::Duration;
use tokio_postgres::types::ToSql;
use tokio_postgres::{Config, NoTls, Transaction};

type PgManager = PostgresConnectionManager<NoTls>;

#[derive(Clone)]
pub(crate) struct PostgresStateStore {
    pool: Pool<PgManager>,
}

impl PostgresStateStore {
    pub async fn connect(database_url: &str) -> io::Result<Self> {
        let mut config = Config::from_str(database_url).map_err(io_other)?;
        if config.get_password().is_none() {
            if let Ok(password) = env::var("PGPASSWORD") {
                config.password(password);
            }
        }

        tracing::info!(
            has_password = config.get_password().is_some(),
            "connecting to postgres state backend"
        );

        let manager = PostgresConnectionManager::new(config, NoTls);
        let pool = Pool::builder()
            .max_size(16)
            .connection_timeout(Duration::from_secs(2))
            .build(manager)
            .await
            .map_err(io_other)?;
        let store = Self { pool };
        store.initialize_schema().await?;
        tracing::info!("postgres state backend initialized");
        Ok(store)
    }

    pub async fn load_state(&self) -> io::Result<PersistedState> {
        let conn = self.pool.get().await.map_err(io_other)?;

        let mut upstreams = Vec::new();
        for row in conn
            .query(
                "SELECT id, name, base_url, api_key, protocol, protocols, \
                 model_contexts, \
                 COALESCE(request_quota_window_hours, 5), \
                 COALESCE(request_quota_requests, request_quota_5h, 600), \
                 requests_per_minute, max_concurrency, priority, premium_only, \
                 protect_premium_quota, active, failure_count \
                 FROM upstreams ORDER BY id",
                &[],
            )
            .await
            .map_err(io_other)?
        {
            let protocol = decode_protocol(row.get::<_, String>(4))?;
            upstreams.push(UpstreamConfig {
                id: row.get::<_, String>(0),
                name: row.get::<_, String>(1),
                base_url: row.get::<_, String>(2),
                api_key: row.get::<_, String>(3),
                protocol,
                protocols: decode_protocols(row.get::<_, Option<String>>(5), protocol)?,
                model_contexts: decode_model_contexts(row.get::<_, Option<String>>(6))?,
                supported_models: Vec::new(),
                request_quota_window_hours: row.get::<_, i32>(7) as u32,
                request_quota_requests: row.get::<_, i32>(8) as u32,
                requests_per_minute: row.get::<_, i32>(9) as u32,
                max_concurrency: row.get::<_, i32>(10) as u32,
                priority: row.get::<_, i32>(11) as u32,
                model_request_costs: Vec::new(),
                premium_models: Vec::new(),
                premium_only: row.get::<_, bool>(12),
                protect_premium_quota: row.get::<_, bool>(13),
                active: row.get::<_, bool>(14),
                failure_count: row.get::<_, i32>(15) as u32,
            });
        }

        let mut upstream_index = HashMap::new();
        for (index, upstream) in upstreams.iter().enumerate() {
            upstream_index.insert(upstream.id.clone(), index);
        }

        for row in conn
            .query(
                "SELECT upstream_id, model_slug FROM upstream_supported_models \
                 ORDER BY upstream_id, position, model_slug",
                &[],
            )
            .await
            .map_err(io_other)?
        {
            let upstream_id: String = row.get(0);
            let model_slug: String = row.get(1);
            if let Some(&index) = upstream_index.get(&upstream_id) {
                upstreams[index].supported_models.push(model_slug);
            }
        }

        for row in conn
            .query(
                "SELECT upstream_id, model_slug FROM upstream_premium_models \
                 ORDER BY upstream_id, position, model_slug",
                &[],
            )
            .await
            .map_err(io_other)?
        {
            let upstream_id: String = row.get(0);
            let model_slug: String = row.get(1);
            if let Some(&index) = upstream_index.get(&upstream_id) {
                upstreams[index].premium_models.push(model_slug);
            }
        }

        for row in conn
            .query(
                "SELECT upstream_id, slug, cost FROM upstream_model_request_costs \
                 ORDER BY upstream_id, position, slug",
                &[],
            )
            .await
            .map_err(io_other)?
        {
            let upstream_id: String = row.get(0);
            if let Some(&index) = upstream_index.get(&upstream_id) {
                upstreams[index]
                    .model_request_costs
                    .push(ModelRequestCostConfig {
                        slug: row.get::<_, String>(1),
                        cost: row.get::<_, i32>(2) as f64,
                    });
            }
        }

        let mut downstreams = Vec::new();
        for row in conn
            .query(
                "SELECT id, name, hash, plaintext_key, rate_limit_enabled, per_minute_limit, max_concurrency, \
                 daily_token_limit, monthly_token_limit, request_quota_window_hours, \
                 request_quota_requests, expires_at, active \
                 FROM downstreams ORDER BY id",
                &[],
            )
            .await
            .map_err(io_other)?
        {
            downstreams.push(DownstreamConfig {
                id: row.get::<_, String>(0),
                name: row.get::<_, String>(1),
                hash: row.get::<_, String>(2),
                plaintext_key: row.get::<_, Option<String>>(3),
                plaintext_key_prefix: None,
                model_allowlist: Vec::new(),
                rate_limit_enabled: row.get::<_, bool>(4),
                per_minute_limit: row.get::<_, i32>(5) as u32,
                max_concurrency: row.get::<_, i32>(6) as u32,
                daily_token_limit: row.get::<_, Option<i64>>(7).map(|value| value as u64),
                monthly_token_limit: row.get::<_, Option<i64>>(8).map(|value| value as u64),
                request_quota_window_hours: row.get::<_, Option<i32>>(9).map(|value| value as u32),
                request_quota_requests: row.get::<_, Option<i32>>(10).map(|value| value as u32),
                ip_allowlist: Vec::new(),
                expires_at: row.get::<_, Option<i64>>(11).map(|value| value as u64),
                active: row.get::<_, bool>(12),
            });
        }

        let mut downstream_index = HashMap::new();
        for (index, downstream) in downstreams.iter().enumerate() {
            downstream_index.insert(downstream.id.clone(), index);
        }

        for row in conn
            .query(
                "SELECT downstream_id, model_slug FROM downstream_model_allowlist \
                 ORDER BY downstream_id, position, model_slug",
                &[],
            )
            .await
            .map_err(io_other)?
        {
            let downstream_id: String = row.get(0);
            let model_slug: String = row.get(1);
            if let Some(&index) = downstream_index.get(&downstream_id) {
                downstreams[index].model_allowlist.push(model_slug);
            }
        }

        for row in conn
            .query(
                "SELECT downstream_id, ip_address FROM downstream_ip_allowlist \
                 ORDER BY downstream_id, position, ip_address",
                &[],
            )
            .await
            .map_err(io_other)?
        {
            let downstream_id: String = row.get(0);
            let ip_address: String = row.get(1);
            if let Some(&index) = downstream_index.get(&downstream_id) {
                downstreams[index].ip_allowlist.push(ip_address);
            }
        }

        let mut usage_logs = Vec::new();
        for row in conn
            .query(
                "SELECT id, downstream_key_id, upstream_key_id, downstream_name, upstream_name, \
                 endpoint, model, inference_strength, billing_mode, request_count, user_agent, request_id, \
                 status_code, error_message, error_category, prompt_tokens, completion_tokens, total_tokens, latency_ms, created_at \
                 FROM usage_logs ORDER BY created_at, request_id, id",
                &[],
            )
            .await
            .map_err(io_other)?
        {
            usage_logs.push(UsageLog {
                id: row.get::<_, String>(0),
                downstream_key_id: row.get::<_, String>(1),
                upstream_key_id: row.get::<_, String>(2),
                downstream_name: row.get::<_, Option<String>>(3),
                upstream_name: row.get::<_, Option<String>>(4),
                endpoint: row.get::<_, String>(5),
                model: row.get::<_, String>(6),
                inference_strength: row.get::<_, Option<String>>(7),
                billing_mode: row.get::<_, Option<String>>(8),
                request_count: row.get::<_, Option<i64>>(9).map(|value| value as u64),
                user_agent: row.get::<_, Option<String>>(10),
                request_id: row.get::<_, String>(11),
                status_code: row.get::<_, i32>(12) as u16,
                error_message: row.get::<_, Option<String>>(13),
                error_category: row.get::<_, Option<String>>(14),
                prompt_tokens: row.get::<_, i64>(15) as u64,
                completion_tokens: row.get::<_, i64>(16) as u64,
                total_tokens: row.get::<_, i64>(17) as u64,
                latency_ms: row.get::<_, i64>(18) as u64,
                created_at: row.get::<_, i64>(19) as u64,
            });
        }

        Ok(PersistedState {
            upstreams,
            downstreams,
            usage_logs,
        })
    }

    pub async fn replace_state(&self, state: &PersistedState) -> io::Result<()> {
        let mut conn = self.pool.get().await.map_err(io_other)?;
        let tx = conn.transaction().await.map_err(io_other)?;
        sync_config_tables(&tx, state).await?;
        insert_usage_logs(&tx, &state.usage_logs).await?;
        tx.commit().await.map_err(io_other)
    }

    pub async fn append_usage_logs(&self, logs: &[UsageLog]) -> io::Result<()> {
        if logs.is_empty() {
            return Ok(());
        }

        let mut conn = self.pool.get().await.map_err(io_other)?;
        let tx = conn.transaction().await.map_err(io_other)?;
        insert_usage_logs(&tx, logs).await?;
        tx.commit().await.map_err(io_other)
    }

    async fn initialize_schema(&self) -> io::Result<()> {
        let conn = self.pool.get().await.map_err(io_other)?;
        conn.batch_execute(SCHEMA_SQL).await.map_err(io_other)
    }
}

async fn sync_config_tables(tx: &Transaction<'_>, state: &PersistedState) -> io::Result<()> {
    sync_upstreams(tx, &state.upstreams).await?;
    sync_downstreams(tx, &state.downstreams).await
}

async fn sync_upstreams(tx: &Transaction<'_>, upstreams: &[UpstreamConfig]) -> io::Result<()> {
    let desired_ids = upstreams
        .iter()
        .map(|upstream| upstream.id.as_str())
        .collect::<HashSet<_>>();
    let existing_rows = tx
        .query("SELECT id FROM upstreams", &[])
        .await
        .map_err(io_other)?;
    for row in existing_rows {
        let id: String = row.get(0);
        if !desired_ids.contains(id.as_str()) {
            tx.execute("DELETE FROM upstreams WHERE id = $1", &[&id])
                .await
                .map_err(io_other)?;
        }
    }

    for upstream in upstreams {
        let protocols = upstream.supported_protocols();
        let primary_protocol = protocols
            .first()
            .copied()
            .unwrap_or(UpstreamProtocol::ChatCompletions);
        let protocol_text = format!("{:?}", primary_protocol);
        let protocols_json = encode_protocols(&protocols);
        let model_contexts_json = encode_model_contexts(&upstream.model_contexts);
        let params: &[&(dyn ToSql + Sync)] = &[
            &upstream.id,
            &upstream.name,
            &upstream.base_url,
            &upstream.api_key,
            &protocol_text,
            &protocols_json,
            &model_contexts_json,
            &(upstream.request_quota_requests as i32),
            &(upstream.request_quota_window_hours as i32),
            &(upstream.request_quota_requests as i32),
            &(upstream.requests_per_minute as i32),
            &(upstream.max_concurrency as i32),
            &(upstream.priority as i32),
            &upstream.premium_only,
            &upstream.protect_premium_quota,
            &upstream.active,
            &(upstream.failure_count as i32),
        ];
        tx.execute(
            "INSERT INTO upstreams (
                id, name, base_url, api_key, protocol, protocols, model_contexts,
                request_quota_5h, request_quota_window_hours, request_quota_requests,
                requests_per_minute, max_concurrency, priority, premium_only,
                protect_premium_quota, active, failure_count
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7,
                $8, $9, $10,
                $11, $12, $13, $14,
                $15, $16, $17
            )
            ON CONFLICT (id) DO UPDATE SET
                name = EXCLUDED.name,
                base_url = EXCLUDED.base_url,
                api_key = EXCLUDED.api_key,
                protocol = EXCLUDED.protocol,
                protocols = EXCLUDED.protocols,
                model_contexts = EXCLUDED.model_contexts,
                request_quota_5h = EXCLUDED.request_quota_5h,
                request_quota_window_hours = EXCLUDED.request_quota_window_hours,
                request_quota_requests = EXCLUDED.request_quota_requests,
                requests_per_minute = EXCLUDED.requests_per_minute,
                max_concurrency = EXCLUDED.max_concurrency,
                priority = EXCLUDED.priority,
                premium_only = EXCLUDED.premium_only,
                protect_premium_quota = EXCLUDED.protect_premium_quota,
                active = EXCLUDED.active,
                failure_count = EXCLUDED.failure_count",
            params,
        )
        .await
        .map_err(io_other)?;

        tx.execute(
            "DELETE FROM upstream_supported_models WHERE upstream_id = $1",
            &[&upstream.id],
        )
        .await
        .map_err(io_other)?;
        for (position, model_slug) in upstream.supported_models.iter().enumerate() {
            let params: &[&(dyn ToSql + Sync)] =
                &[&upstream.id, &(position as i32), model_slug];
            tx.execute(
                "INSERT INTO upstream_supported_models (upstream_id, position, model_slug)
                 VALUES ($1, $2, $3)",
                params,
            )
            .await
            .map_err(io_other)?;
        }

        tx.execute(
            "DELETE FROM upstream_premium_models WHERE upstream_id = $1",
            &[&upstream.id],
        )
        .await
        .map_err(io_other)?;
        for (position, model_slug) in upstream.premium_models.iter().enumerate() {
            let params: &[&(dyn ToSql + Sync)] =
                &[&upstream.id, &(position as i32), model_slug];
            tx.execute(
                "INSERT INTO upstream_premium_models (upstream_id, position, model_slug)
                 VALUES ($1, $2, $3)",
                params,
            )
            .await
            .map_err(io_other)?;
        }

        tx.execute(
            "DELETE FROM upstream_model_request_costs WHERE upstream_id = $1",
            &[&upstream.id],
        )
        .await
        .map_err(io_other)?;
        for (position, rule) in upstream.model_request_costs.iter().enumerate() {
            let cost = rule.cost as i32;
            let params: &[&(dyn ToSql + Sync)] =
                &[&upstream.id, &(position as i32), &rule.slug, &cost];
            tx.execute(
                "INSERT INTO upstream_model_request_costs (upstream_id, position, slug, cost)
                 VALUES ($1, $2, $3, $4)",
                params,
            )
            .await
            .map_err(io_other)?;
        }
    }

    Ok(())
}

async fn sync_downstreams(tx: &Transaction<'_>, downstreams: &[DownstreamConfig]) -> io::Result<()> {
    let desired_ids = downstreams
        .iter()
        .map(|downstream| downstream.id.as_str())
        .collect::<HashSet<_>>();
    let existing_rows = tx
        .query("SELECT id FROM downstreams", &[])
        .await
        .map_err(io_other)?;
    for row in existing_rows {
        let id: String = row.get(0);
        if !desired_ids.contains(id.as_str()) {
            tx.execute("DELETE FROM downstreams WHERE id = $1", &[&id])
                .await
                .map_err(io_other)?;
        }
    }

    for downstream in downstreams {
        let daily_token_limit = downstream.daily_token_limit.map(|value| value as i64);
        let monthly_token_limit = downstream.monthly_token_limit.map(|value| value as i64);
        let request_quota_window_hours =
            downstream.request_quota_window_hours.map(|value| value as i32);
        let request_quota_requests = downstream.request_quota_requests.map(|value| value as i32);
        let expires_at = downstream.expires_at.map(|value| value as i64);
        let params: &[&(dyn ToSql + Sync)] = &[
            &downstream.id,
            &downstream.name,
            &downstream.hash,
            &downstream.plaintext_key,
            &downstream.rate_limit_enabled,
            &(downstream.per_minute_limit as i32),
            &(downstream.max_concurrency as i32),
            &daily_token_limit,
            &monthly_token_limit,
            &request_quota_window_hours,
            &request_quota_requests,
            &expires_at,
            &downstream.active,
        ];

        tx.execute(
            "INSERT INTO downstreams (
                id, name, hash, plaintext_key, rate_limit_enabled, per_minute_limit,
                max_concurrency, daily_token_limit, monthly_token_limit,
                request_quota_window_hours, request_quota_requests, expires_at, active
            ) VALUES (
                $1, $2, $3, $4, $5, $6,
                $7, $8, $9,
                $10, $11, $12, $13
            )
            ON CONFLICT (id) DO UPDATE SET
                name = EXCLUDED.name,
                hash = EXCLUDED.hash,
                plaintext_key = EXCLUDED.plaintext_key,
                rate_limit_enabled = EXCLUDED.rate_limit_enabled,
                per_minute_limit = EXCLUDED.per_minute_limit,
                max_concurrency = EXCLUDED.max_concurrency,
                daily_token_limit = EXCLUDED.daily_token_limit,
                monthly_token_limit = EXCLUDED.monthly_token_limit,
                request_quota_window_hours = EXCLUDED.request_quota_window_hours,
                request_quota_requests = EXCLUDED.request_quota_requests,
                expires_at = EXCLUDED.expires_at,
                active = EXCLUDED.active",
            params,
        )
        .await
        .map_err(io_other)?;

        tx.execute(
            "DELETE FROM downstream_model_allowlist WHERE downstream_id = $1",
            &[&downstream.id],
        )
        .await
        .map_err(io_other)?;
        for (position, model_slug) in downstream.model_allowlist.iter().enumerate() {
            let params: &[&(dyn ToSql + Sync)] =
                &[&downstream.id, &(position as i32), model_slug];
            tx.execute(
                "INSERT INTO downstream_model_allowlist (downstream_id, position, model_slug)
                 VALUES ($1, $2, $3)",
                params,
            )
            .await
            .map_err(io_other)?;
        }

        tx.execute(
            "DELETE FROM downstream_ip_allowlist WHERE downstream_id = $1",
            &[&downstream.id],
        )
        .await
        .map_err(io_other)?;
        for (position, ip_address) in downstream.ip_allowlist.iter().enumerate() {
            let params: &[&(dyn ToSql + Sync)] =
                &[&downstream.id, &(position as i32), ip_address];
            tx.execute(
                "INSERT INTO downstream_ip_allowlist (downstream_id, position, ip_address)
                 VALUES ($1, $2, $3)",
                params,
            )
            .await
            .map_err(io_other)?;
        }
    }

    Ok(())
}

async fn insert_usage_logs(tx: &Transaction<'_>, logs: &[UsageLog]) -> io::Result<()> {
    for log in logs {
        let request_count = log.request_count.map(|value| value as i64);
        let prompt_tokens = log.prompt_tokens as i64;
        let completion_tokens = log.completion_tokens as i64;
        let total_tokens = log.total_tokens as i64;
        let latency_ms = log.latency_ms as i64;
        let created_at = log.created_at as i64;
        let status_code = log.status_code as i32;
        let params: &[&(dyn ToSql + Sync)] = &[
            &log.id,
            &log.downstream_key_id,
            &log.upstream_key_id,
            &log.downstream_name,
            &log.upstream_name,
            &log.endpoint,
            &log.model,
            &log.inference_strength,
            &log.billing_mode,
            &request_count,
            &log.user_agent,
            &log.request_id,
            &status_code,
            &log.error_message,
            &log.error_category,
            &prompt_tokens,
            &completion_tokens,
            &total_tokens,
            &latency_ms,
            &created_at,
        ];

        tx.execute(
            "INSERT INTO usage_logs (
                id, downstream_key_id, upstream_key_id, downstream_name, upstream_name,
                endpoint, model, inference_strength, billing_mode, request_count,
                user_agent, request_id, status_code, error_message, error_category,
                prompt_tokens, completion_tokens, total_tokens, latency_ms, created_at
            ) VALUES (
                $1, $2, $3, $4, $5,
                $6, $7, $8, $9, $10,
                $11, $12, $13, $14, $15,
                $16, $17, $18, $19, $20
            ) ON CONFLICT (id) DO NOTHING",
            params,
        )
        .await
        .map_err(io_other)?;
    }

    Ok(())
}

fn decode_protocol(value: String) -> io::Result<UpstreamProtocol> {
    serde_json::from_value(serde_json::Value::String(value))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn decode_protocols(
    value: Option<String>,
    fallback: UpstreamProtocol,
) -> io::Result<Vec<UpstreamProtocol>> {
    let Some(value) = value.map(|value| value.trim().to_string()).filter(|value| !value.is_empty())
    else {
        return Ok(vec![fallback]);
    };

    if let Ok(protocols) = serde_json::from_str::<Vec<UpstreamProtocol>>(&value) {
        return Ok(protocols);
    }

    decode_protocol(value).map(|protocol| vec![protocol])
}

fn encode_protocols(protocols: &[UpstreamProtocol]) -> String {
    serde_json::to_string(protocols).unwrap_or_else(|_| "[]".to_string())
}

fn decode_model_contexts(value: Option<String>) -> io::Result<Vec<ModelContextConfig>> {
    let Some(value) = value.map(|value| value.trim().to_string()).filter(|value| !value.is_empty())
    else {
        return Ok(Vec::new());
    };

    serde_json::from_str::<Vec<ModelContextConfig>>(&value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn encode_model_contexts(values: &[ModelContextConfig]) -> String {
    serde_json::to_string(values).unwrap_or_else(|_| "[]".to_string())
}

fn io_other<E>(error: E) -> io::Error
where
    E: std::error::Error + Send + Sync + 'static,
{
    io::Error::new(io::ErrorKind::Other, error)
}

const SCHEMA_SQL: &str = r#"
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
ALTER TABLE upstreams
    ADD COLUMN IF NOT EXISTS protocols TEXT NULL;
ALTER TABLE upstreams
    ADD COLUMN IF NOT EXISTS model_contexts TEXT NULL;

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
    error_message TEXT NULL,
    error_category TEXT NULL,
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
ALTER TABLE usage_logs
    ADD COLUMN IF NOT EXISTS error_message TEXT NULL;
ALTER TABLE usage_logs
    ADD COLUMN IF NOT EXISTS error_category TEXT NULL;

CREATE INDEX IF NOT EXISTS usage_logs_created_at_idx
    ON usage_logs (created_at DESC, id);
CREATE INDEX IF NOT EXISTS usage_logs_downstream_idx
    ON usage_logs (downstream_key_id, created_at DESC);
CREATE INDEX IF NOT EXISTS usage_logs_upstream_idx
    ON usage_logs (upstream_key_id, created_at DESC);
"#;
