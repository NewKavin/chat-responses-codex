use super::log_queries::{current_month_start, enrich_usage_log, query_time_bounds};
use super::{
    unix_seconds, AnnouncementConfig, AnnouncementLevel, DownstreamConfig, DownstreamUsageSummary,
    DefaultModelContextConfig, GlobalContextProfile, ModelContextConfig, ModelRequestCostConfig,
    PersistedState, UpstreamConfig,
    UpstreamProtocol,
    UsageLog, UsageLogPage, UsageLogQuery,
};
use bb8::Pool;
use bb8_postgres::PostgresConnectionManager;
use std::collections::{HashMap, HashSet};
use std::env;
use std::io;
use std::str::FromStr;
use std::time::Duration;
use tokio_postgres::types::ToSql;
use tokio_postgres::{Config, NoTls, Row, Transaction};

type PgManager = PostgresConnectionManager<NoTls>;
const POSTGRES_RUNTIME_USAGE_LOG_WINDOW_DAYS: u64 = 32;

#[derive(Clone)]
pub(crate) struct PostgresStateStore {
    pool: Pool<PgManager>,
}

impl PostgresStateStore {
    pub async fn connect(database_url: &str, pool_max_size: u32) -> io::Result<Self> {
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
            .max_size(pool_max_size.max(1))
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
                 model_contexts, default_model_context, \
                 COALESCE(request_quota_window_hours, 5), \
                 COALESCE(request_quota_requests, request_quota_5h, 600), \
                 requests_per_minute, max_concurrency, priority, premium_only, \
                 protect_premium_quota, active, failure_count, \
                 auto_managed, managed_source, last_synced_at \
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
                api_keys: Vec::new(),
                protocol,
                protocols: decode_protocols(row.get::<_, Option<String>>(5), protocol)?,
                model_contexts: decode_model_contexts(row.get::<_, Option<String>>(6))?,
                default_model_context: decode_default_model_context(row.get::<_, Option<String>>(7))?,
                supported_models: Vec::new(),
                request_quota_window_hours: row.get::<_, i32>(8) as u32,
                request_quota_requests: row.get::<_, i32>(9) as u32,
                requests_per_minute: row.get::<_, i32>(10) as u32,
                max_concurrency: row.get::<_, i32>(11) as u32,
                priority: row.get::<_, i32>(12) as u32,
                model_request_costs: Vec::new(),
                premium_models: Vec::new(),
                premium_only: row.get::<_, bool>(13),
                protect_premium_quota: row.get::<_, bool>(14),
                active: row.get::<_, bool>(15),
                failure_count: row.get::<_, i32>(16) as u32,
                auto_managed: row.get::<_, bool>(17),
                managed_source: row.get::<_, Option<String>>(18),
                last_synced_at: row.get::<_, i64>(19) as u64,
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

        let mut global_context_profiles = HashMap::new();
        for row in conn
            .query(
                "SELECT upstream_base_url, model_contexts, default_model_context \
                 FROM global_context_profiles ORDER BY upstream_base_url",
                &[],
            )
            .await
            .map_err(io_other)?
        {
            let base_url: String = row.get(0);
            let base_url = base_url.trim().trim_end_matches('/').to_string();
            if base_url.is_empty() {
                continue;
            }

            let mut profile = GlobalContextProfile {
                model_contexts: decode_model_contexts(row.get(1))?,
                default_model_context: decode_default_model_context(row.get(2))?,
            };
            profile.normalize_for_storage();
            global_context_profiles.insert(base_url, profile);
        }

        let runtime_usage_start = runtime_usage_log_start(unix_seconds());
        let mut usage_logs = Vec::new();
        for row in conn
            .query(
                "SELECT id, downstream_key_id, upstream_key_id, downstream_name, upstream_name, \
                 endpoint, model, inference_strength, billing_mode, request_count, user_agent, request_id, \
                 status_code, error_message, error_category, prompt_tokens, completion_tokens, total_tokens, latency_ms, created_at \
                 FROM usage_logs WHERE created_at >= $1 ORDER BY created_at, request_id, id",
                &[&runtime_usage_start],
            )
            .await
            .map_err(io_other)?
        {
            usage_logs.push(usage_log_from_row(&row));
        }

        let announcement = load_announcement(&conn).await?;

        Ok(PersistedState {
            upstreams,
            downstreams,
            global_context_profiles,
            usage_logs,
            announcement,
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

    pub async fn query_usage_logs_page(
        &self,
        query: &UsageLogQuery,
    ) -> io::Result<Option<UsageLogPage>> {
        let conn = self.pool.get().await.map_err(io_other)?;
        let (start_time, end_time) = query_time_bounds(query, unix_seconds());
        let start_time = u64_to_i64(start_time);
        let end_time = u64_to_i64(end_time);
        let status_codes = query
            .status_codes
            .iter()
            .map(|status_code| i32::from(*status_code))
            .collect::<Vec<_>>();
        let no_status_filter = status_codes.is_empty();
        let model_substring = query
            .model_substring
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_ascii_lowercase());

        let total_row = conn
            .query_one(
                "SELECT COUNT(*)::BIGINT
                 FROM usage_logs
                 WHERE created_at >= $1
                   AND created_at <= $2
                   AND ($3 OR status_code = ANY($4))
                   AND ($5::TEXT IS NULL OR POSITION($5 IN LOWER(TRIM(model))) > 0)",
                &[
                    &start_time,
                    &end_time,
                    &no_status_filter,
                    &status_codes,
                    &model_substring,
                ],
            )
            .await
            .map_err(io_other)?;
        let total = i64_to_usize(total_row.get::<_, i64>(0));
        let page_size = query.page_size.max(1);
        let page = query.page.max(1);
        let total_pages = total.div_ceil(page_size);
        let offset = page
            .saturating_sub(1)
            .saturating_mul(page_size)
            .min(i64::MAX as usize) as i64;
        let limit = page_size.min(i64::MAX as usize) as i64;

        let rows = conn
            .query(
                "SELECT id, downstream_key_id, upstream_key_id, downstream_name, upstream_name,
                        endpoint, model, inference_strength, billing_mode, request_count, user_agent, request_id,
                        status_code, error_message, error_category, prompt_tokens, completion_tokens, total_tokens, latency_ms, created_at
                 FROM usage_logs
                 WHERE created_at >= $1
                   AND created_at <= $2
                   AND ($3 OR status_code = ANY($4))
                   AND ($5::TEXT IS NULL OR POSITION($5 IN LOWER(TRIM(model))) > 0)
                 ORDER BY created_at DESC, request_id ASC, id ASC
                 LIMIT $6 OFFSET $7",
                &[
                    &start_time,
                    &end_time,
                    &no_status_filter,
                    &status_codes,
                    &model_substring,
                    &limit,
                    &offset,
                ],
            )
            .await
            .map_err(io_other)?;
        let logs = rows
            .iter()
            .map(usage_log_from_row)
            .map(|log| enrich_usage_log(&log))
            .collect();

        Ok(Some(UsageLogPage {
            logs,
            total,
            page,
            page_size,
            total_pages,
        }))
    }

    pub async fn downstream_usage_summary(
        &self,
        downstream_id: &str,
    ) -> io::Result<Option<DownstreamUsageSummary>> {
        let conn = self.pool.get().await.map_err(io_other)?;
        let downstream_row = conn
            .query_opt(
                "SELECT id FROM downstreams WHERE id = $1",
                &[&downstream_id],
            )
            .await
            .map_err(io_other)?;
        if downstream_row.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("downstream not found: {downstream_id}"),
            ));
        }

        let allowlist_count = conn
            .query_one(
                "SELECT COUNT(DISTINCT LOWER(TRIM(model_slug)))::BIGINT
                 FROM downstream_model_allowlist
                 WHERE downstream_id = $1
                   AND TRIM(model_slug) <> ''",
                &[&downstream_id],
            )
            .await
            .map_err(io_other)?
            .get::<_, i64>(0);
        let total_models = if allowlist_count > 0 {
            i64_to_usize(allowlist_count)
        } else {
            let row = conn
                .query_one(
                    "SELECT COUNT(DISTINCT LOWER(TRIM(model_slug)))::BIGINT
                     FROM (
                         SELECT supported.model_slug
                         FROM upstream_supported_models supported
                         JOIN upstreams upstream ON upstream.id = supported.upstream_id
                         WHERE upstream.active
                         UNION
                         SELECT premium.model_slug
                         FROM upstream_premium_models premium
                         JOIN upstreams upstream ON upstream.id = premium.upstream_id
                         WHERE upstream.active
                     ) route_models",
                    &[],
                )
                .await
                .map_err(io_other)?;
            i64_to_usize(row.get::<_, i64>(0))
        };

        let allowlist_empty = allowlist_count == 0;
        let active_models = conn
            .query_one(
                "SELECT COUNT(DISTINCT LOWER(TRIM(usage_logs.model)))::BIGINT
                 FROM usage_logs
                 WHERE usage_logs.downstream_key_id = $1
                   AND (
                       $2
                       OR EXISTS (
                           SELECT 1
                           FROM downstream_model_allowlist allowlist
                           WHERE allowlist.downstream_id = $1
                             AND LOWER(TRIM(allowlist.model_slug)) = LOWER(TRIM(usage_logs.model))
                       )
                   )",
                &[&downstream_id, &allowlist_empty],
            )
            .await
            .map_err(io_other)?
            .get::<_, i64>(0);

        let now = unix_seconds();
        let today_start = u64_to_i64((now / 86_400) * 86_400);
        let month_start = u64_to_i64(current_month_start(now));
        let token_row = conn
            .query_one(
                "SELECT
                     COALESCE(SUM(total_tokens) FILTER (WHERE created_at >= $2), 0)::BIGINT,
                     COALESCE(SUM(total_tokens) FILTER (WHERE created_at >= $3), 0)::BIGINT
                 FROM usage_logs
                 WHERE downstream_key_id = $1",
                &[&downstream_id, &today_start, &month_start],
            )
            .await
            .map_err(io_other)?;

        Ok(Some(DownstreamUsageSummary {
            downstream_id: downstream_id.to_string(),
            today_tokens: i64_to_u64(token_row.get::<_, i64>(0)),
            month_tokens: i64_to_u64(token_row.get::<_, i64>(1)),
            total_models,
            active_models: i64_to_usize(active_models),
        }))
    }

    async fn initialize_schema(&self) -> io::Result<()> {
        let conn = self.pool.get().await.map_err(io_other)?;
        conn.batch_execute(SCHEMA_SQL).await.map_err(io_other)
    }
}

async fn sync_config_tables(tx: &Transaction<'_>, state: &PersistedState) -> io::Result<()> {
    sync_upstreams(tx, &state.upstreams).await?;
    sync_downstreams(tx, &state.downstreams).await?;
    sync_global_context_profiles(tx, &state.global_context_profiles).await?;
    sync_announcements(tx, &state.announcement).await
}

async fn sync_global_context_profiles(
    tx: &Transaction<'_>,
    profiles: &HashMap<String, GlobalContextProfile>,
) -> io::Result<()> {
    let desired_urls = profiles
        .keys()
        .map(|url| url.as_str())
        .collect::<HashSet<_>>();
    let existing_rows = tx
        .query("SELECT upstream_base_url FROM global_context_profiles", &[])
        .await
        .map_err(io_other)?;
    for row in existing_rows {
        let upstream_base_url: String = row.get(0);
        if !desired_urls.contains(upstream_base_url.as_str()) {
            tx.execute(
                "DELETE FROM global_context_profiles WHERE upstream_base_url = $1",
                &[&upstream_base_url],
            )
            .await
            .map_err(io_other)?;
        }
    }

    for (upstream_base_url, profile) in profiles {
        let params: &[&(dyn ToSql + Sync)] = &[
            upstream_base_url,
            &encode_model_contexts(&profile.model_contexts),
            &encode_default_model_context(&profile.default_model_context),
        ];
        tx.execute(
            "INSERT INTO global_context_profiles (
                upstream_base_url, model_contexts, default_model_context
            ) VALUES (
                $1, $2, $3
            )
            ON CONFLICT (upstream_base_url) DO UPDATE SET
                model_contexts = EXCLUDED.model_contexts,
                default_model_context = EXCLUDED.default_model_context",
            params,
        )
        .await
        .map_err(io_other)?;
    }

    Ok(())
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
        let default_model_context_json =
            encode_default_model_context(&upstream.default_model_context);
        let params: &[&(dyn ToSql + Sync)] = &[
            &upstream.id,
            &upstream.name,
            &upstream.base_url,
            &upstream.api_key,
            &protocol_text,
            &protocols_json,
            &model_contexts_json,
            &(upstream.request_quota_requests as i32),
            &default_model_context_json,
            &(upstream.request_quota_window_hours as i32),
            &(upstream.request_quota_requests as i32),
            &(upstream.requests_per_minute as i32),
            &(upstream.max_concurrency as i32),
            &(upstream.priority as i32),
            &upstream.premium_only,
            &upstream.protect_premium_quota,
            &upstream.active,
            &(upstream.failure_count as i32),
            &upstream.auto_managed,
            &upstream.managed_source,
            &(upstream.last_synced_at as i64),
        ];
        tx.execute(
            "INSERT INTO upstreams (
                id, name, base_url, api_key, protocol, protocols, model_contexts,
                request_quota_5h, default_model_context, request_quota_window_hours, request_quota_requests,
                requests_per_minute, max_concurrency, priority, premium_only,
                protect_premium_quota, active, failure_count,
                auto_managed, managed_source, last_synced_at
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7,
                $8, $9, $10,
                $11, $12, $13, $14,
                $15, $16, $17, $18, $19, $20, $21
            )
            ON CONFLICT (id) DO UPDATE SET
                name = EXCLUDED.name,
                base_url = EXCLUDED.base_url,
                api_key = EXCLUDED.api_key,
                protocol = EXCLUDED.protocol,
                protocols = EXCLUDED.protocols,
                model_contexts = EXCLUDED.model_contexts,
                default_model_context = EXCLUDED.default_model_context,
                request_quota_5h = EXCLUDED.request_quota_5h,
                request_quota_window_hours = EXCLUDED.request_quota_window_hours,
                request_quota_requests = EXCLUDED.request_quota_requests,
                requests_per_minute = EXCLUDED.requests_per_minute,
                max_concurrency = EXCLUDED.max_concurrency,
                priority = EXCLUDED.priority,
                premium_only = EXCLUDED.premium_only,
                protect_premium_quota = EXCLUDED.protect_premium_quota,
                active = EXCLUDED.active,
                failure_count = EXCLUDED.failure_count,
                auto_managed = EXCLUDED.auto_managed,
                managed_source = EXCLUDED.managed_source,
                last_synced_at = EXCLUDED.last_synced_at",
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
            let params: &[&(dyn ToSql + Sync)] = &[&upstream.id, &(position as i32), model_slug];
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
            let params: &[&(dyn ToSql + Sync)] = &[&upstream.id, &(position as i32), model_slug];
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

async fn sync_downstreams(
    tx: &Transaction<'_>,
    downstreams: &[DownstreamConfig],
) -> io::Result<()> {
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
        let request_quota_window_hours = downstream
            .request_quota_window_hours
            .map(|value| value as i32);
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
            let params: &[&(dyn ToSql + Sync)] = &[&downstream.id, &(position as i32), model_slug];
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
            let params: &[&(dyn ToSql + Sync)] = &[&downstream.id, &(position as i32), ip_address];
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

async fn sync_announcements(
    tx: &Transaction<'_>,
    announcement: &Option<AnnouncementConfig>,
) -> io::Result<()> {
    match announcement {
        Some(announcement) => {
            let level = match announcement.level {
                AnnouncementLevel::Info => "info",
                AnnouncementLevel::Success => "success",
                AnnouncementLevel::Warning => "warning",
                AnnouncementLevel::Error => "error",
            };
            let updated_at = announcement.updated_at as i64;
            let params: &[&(dyn ToSql + Sync)] = &[
                &announcement.id,
                &announcement.title,
                &announcement.content,
                &level,
                &announcement.active,
                &updated_at,
            ];
            tx.execute(
                "INSERT INTO app_announcements (
                    singleton_id, announcement_id, title, content, level, active, updated_at
                ) VALUES (
                    'global', $1, $2, $3, $4, $5, $6
                )
                ON CONFLICT (singleton_id) DO UPDATE SET
                    announcement_id = EXCLUDED.announcement_id,
                    title = EXCLUDED.title,
                    content = EXCLUDED.content,
                    level = EXCLUDED.level,
                    active = EXCLUDED.active,
                    updated_at = EXCLUDED.updated_at",
                params,
            )
            .await
            .map_err(io_other)?;
        }
        None => {
            tx.execute(
                "DELETE FROM app_announcements WHERE singleton_id = 'global'",
                &[],
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

async fn load_announcement(
    conn: &tokio_postgres::Client,
) -> io::Result<Option<AnnouncementConfig>> {
    let row = conn
        .query_opt(
            "SELECT announcement_id, title, content, level, active, updated_at
             FROM app_announcements
             WHERE singleton_id = 'global'",
            &[],
        )
        .await
        .map_err(io_other)?;

    let Some(row) = row else {
        return Ok(None);
    };

    let level_text: String = row.get(3);
    let level = match level_text.as_str() {
        "info" => AnnouncementLevel::Info,
        "success" => AnnouncementLevel::Success,
        "warning" => AnnouncementLevel::Warning,
        "error" => AnnouncementLevel::Error,
        other => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid announcement level: {other}"),
            ))
        }
    };

    Ok(Some(AnnouncementConfig {
        id: row.get(0),
        title: row.get(1),
        content: row.get(2),
        level,
        active: row.get(4),
        updated_at: row.get::<_, i64>(5).max(0) as u64,
    }))
}

fn usage_log_from_row(row: &Row) -> UsageLog {
    UsageLog {
        id: row.get::<_, String>(0),
        downstream_key_id: row.get::<_, String>(1),
        upstream_key_id: row.get::<_, String>(2),
        downstream_name: row.get::<_, Option<String>>(3),
        upstream_name: row.get::<_, Option<String>>(4),
        endpoint: row.get::<_, String>(5),
        model: row.get::<_, String>(6),
        inference_strength: row.get::<_, Option<String>>(7),
        billing_mode: row.get::<_, Option<String>>(8),
        request_count: row.get::<_, Option<i64>>(9).map(i64_to_u64),
        user_agent: row.get::<_, Option<String>>(10),
        request_id: row.get::<_, String>(11),
        status_code: row.get::<_, i32>(12).clamp(0, u16::MAX as i32) as u16,
        error_message: row.get::<_, Option<String>>(13),
        error_category: row.get::<_, Option<String>>(14),
        prompt_tokens: i64_to_u64(row.get::<_, i64>(15)),
        completion_tokens: i64_to_u64(row.get::<_, i64>(16)),
        total_tokens: i64_to_u64(row.get::<_, i64>(17)),
        latency_ms: i64_to_u64(row.get::<_, i64>(18)),
        created_at: i64_to_u64(row.get::<_, i64>(19)),
    }
}

fn runtime_usage_log_start(now: u64) -> i64 {
    let window_start = now.saturating_sub(POSTGRES_RUNTIME_USAGE_LOG_WINDOW_DAYS * 86_400);
    u64_to_i64(window_start.min(current_month_start(now)))
}

fn u64_to_i64(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}

fn i64_to_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn i64_to_usize(value: i64) -> usize {
    usize::try_from(value).unwrap_or(0)
}

fn decode_protocol(value: String) -> io::Result<UpstreamProtocol> {
    serde_json::from_value(serde_json::Value::String(value))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn decode_protocols(
    value: Option<String>,
    fallback: UpstreamProtocol,
) -> io::Result<Vec<UpstreamProtocol>> {
    let Some(value) = value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
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
    let Some(value) = value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return Ok(Vec::new());
    };

    serde_json::from_str::<Vec<ModelContextConfig>>(&value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn encode_model_contexts(values: &[ModelContextConfig]) -> String {
    serde_json::to_string(values).unwrap_or_else(|_| "[]".to_string())
}

fn decode_default_model_context(
    value: Option<String>,
) -> io::Result<Option<DefaultModelContextConfig>> {
    let Some(value) = value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };

    serde_json::from_str::<DefaultModelContextConfig>(&value)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn encode_default_model_context(
    value: &Option<DefaultModelContextConfig>,
) -> Option<String> {
    value
        .as_ref()
        .and_then(|context| serde_json::to_string(context).ok())
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
ALTER TABLE upstreams
    ADD COLUMN IF NOT EXISTS default_model_context TEXT NULL;
ALTER TABLE upstreams
    ADD COLUMN IF NOT EXISTS auto_managed BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE upstreams
    ADD COLUMN IF NOT EXISTS managed_source TEXT NULL;
ALTER TABLE upstreams
    ADD COLUMN IF NOT EXISTS last_synced_at BIGINT NOT NULL DEFAULT 0;

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

CREATE TABLE IF NOT EXISTS global_context_profiles (
    upstream_base_url TEXT PRIMARY KEY,
    model_contexts TEXT NULL,
    default_model_context TEXT NULL
);

CREATE TABLE IF NOT EXISTS app_announcements (
    singleton_id TEXT PRIMARY KEY,
    announcement_id TEXT NOT NULL,
    title TEXT NOT NULL,
    content TEXT NOT NULL,
    level TEXT NOT NULL,
    active BOOLEAN NOT NULL,
    updated_at BIGINT NOT NULL
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
CREATE INDEX IF NOT EXISTS usage_logs_created_request_id_idx
    ON usage_logs (created_at DESC, request_id, id);
CREATE INDEX IF NOT EXISTS usage_logs_status_created_at_idx
    ON usage_logs (status_code, created_at DESC, request_id, id);
CREATE INDEX IF NOT EXISTS usage_logs_downstream_idx
    ON usage_logs (downstream_key_id, created_at DESC);
CREATE INDEX IF NOT EXISTS usage_logs_upstream_idx
    ON usage_logs (upstream_key_id, created_at DESC);
"#;
