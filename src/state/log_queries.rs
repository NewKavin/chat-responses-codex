use super::{unix_seconds, AppState, PersistedState, UsageLog};
use chrono::Datelike;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io;
use std::time::{Duration, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageLogQuery {
    pub page: usize,
    pub page_size: usize,
    pub status_codes: Vec<u16>,
    pub model_substring: Option<String>,
    pub start_time: Option<u64>,
    pub end_time: Option<u64>,
}

impl Default for UsageLogQuery {
    fn default() -> Self {
        Self {
            page: 1,
            page_size: 10,
            status_codes: Vec::new(),
            model_substring: None,
            start_time: None,
            end_time: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichedUsageLog {
    #[serde(flatten)]
    pub log: UsageLog,
    pub api_name: String,
    pub inference_strength: String,
    pub log_type: String,
    pub billing_mode: String,
    pub request_count: u64,
    pub user_agent: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageLogPage {
    pub logs: Vec<EnrichedUsageLog>,
    pub total: usize,
    pub page: usize,
    pub page_size: usize,
    pub total_pages: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownstreamUsageSummary {
    pub downstream_id: String,
    pub today_tokens: u64,
    pub month_tokens: u64,
    pub total_models: usize,
    pub active_models: usize,
}

fn resolve_api_name_and_type(endpoint: &str) -> (&'static str, &'static str) {
    let lower = endpoint.to_ascii_lowercase();
    if lower.contains("/files") && (lower.contains("/content") || lower.contains("/download")) {
        return ("文件下载", "文件");
    }
    if lower.contains("/files") || lower.contains("/upload") {
        return ("文件上传", "文件");
    }
    if lower.contains("/responses") {
        return ("Responses API", "推理");
    }
    if lower.contains("/chat/completions") {
        return ("ChatCompletions API", "对话");
    }
    if lower.contains("/embeddings") {
        return ("Embeddings API", "向量");
    }
    ("通用 API", "其它")
}

pub(crate) fn enrich_usage_log(log: &UsageLog) -> EnrichedUsageLog {
    let (api_name, log_type) = resolve_api_name_and_type(&log.endpoint);
    let inference_strength = log
        .inference_strength
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("标准")
        .to_string();
    let billing_mode = log
        .billing_mode
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            if log.total_tokens > 0 {
                "Token 计费".to_string()
            } else {
                "请求计费".to_string()
            }
        });
    let request_count = log.request_count.unwrap_or(1);
    let user_agent = log
        .user_agent
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("未采集")
        .to_string();

    EnrichedUsageLog {
        log: log.clone(),
        api_name: api_name.to_string(),
        inference_strength,
        log_type: log_type.to_string(),
        billing_mode,
        request_count,
        user_agent,
    }
}

pub(crate) fn query_time_bounds(query: &UsageLogQuery, now: u64) -> (u64, u64) {
    if query.start_time.is_some() || query.end_time.is_some() {
        let start = query.start_time.unwrap_or(0);
        let end = query.end_time.unwrap_or(now);
        if start <= end {
            (start, end)
        } else {
            (end, start)
        }
    } else {
        (now.saturating_sub(7 * 86_400), now)
    }
}

pub(crate) fn current_month_start(now: u64) -> u64 {
    let dt = UNIX_EPOCH + Duration::from_secs(now);
    let datetime = chrono::DateTime::<chrono::Utc>::from(dt);
    let first_of_month = datetime.date_naive().with_day(1).unwrap();
    first_of_month
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp() as u64
}

pub fn build_downstream_usage_summary(
    snapshot: &PersistedState,
    downstream_id: &str,
    now: u64,
) -> io::Result<DownstreamUsageSummary> {
    let downstream = snapshot
        .downstreams
        .iter()
        .find(|downstream| downstream.id == downstream_id)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("downstream not found: {downstream_id}"),
            )
        })?;

    let today_start = (now / 86_400) * 86_400;
    let month_start = current_month_start(now);

    let downstream_logs = snapshot
        .usage_logs
        .iter()
        .filter(|log| log.downstream_key_id == downstream_id)
        .collect::<Vec<_>>();

    let today_tokens = downstream_logs
        .iter()
        .filter(|log| log.created_at >= today_start)
        .map(|log| log.total_tokens)
        .sum();
    let month_tokens = downstream_logs
        .iter()
        .filter(|log| log.created_at >= month_start)
        .map(|log| log.total_tokens)
        .sum();

    let total_models = if !downstream.model_allowlist.is_empty() {
        downstream.model_allowlist.len()
    } else {
        snapshot
            .upstreams
            .iter()
            .filter(|upstream| upstream.active)
            .flat_map(|upstream| upstream.route_models())
            .collect::<HashSet<_>>()
            .len()
    };

    let active_models = downstream_logs
        .iter()
        .filter(|log| {
            downstream.model_allowlist.is_empty()
                || downstream.model_allowlist.contains(&log.model)
        })
        .map(|log| log.model.as_str())
        .collect::<HashSet<_>>()
        .len();

    Ok(DownstreamUsageSummary {
        downstream_id: downstream.id.clone(),
        today_tokens,
        month_tokens,
        total_models,
        active_models,
    })
}

impl AppState {
    pub async fn query_usage_logs_page(&self, query: UsageLogQuery) -> io::Result<UsageLogPage> {
        let query = UsageLogQuery {
            page: query.page.max(1),
            page_size: query
                .page_size
                .clamp(1, self.config.admin_logs_page_size_max.max(1)),
            status_codes: query.status_codes,
            model_substring: query.model_substring,
            start_time: query.start_time,
            end_time: query.end_time,
        };

        if let Some(page) = self.config_store.query_usage_logs_page(&query).await? {
            return Ok(page);
        }

        let snapshot = self.snapshot().await;
        let now = unix_seconds();
        let (start_time, end_time) = query_time_bounds(&query, now);
        let model_substring = query
            .model_substring
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_ascii_lowercase());

        let mut logs = snapshot
            .usage_logs
            .into_iter()
            .filter(|log| {
                if log.created_at < start_time || log.created_at > end_time {
                    return false;
                }

                if !query.status_codes.is_empty() && !query.status_codes.contains(&log.status_code)
                {
                    return false;
                }

                if let Some(model_substring) = &model_substring {
                    if !log
                        .model
                        .to_ascii_lowercase()
                        .contains(model_substring)
                    {
                        return false;
                    }
                }

                true
            })
            .collect::<Vec<_>>();

        logs.sort_by_key(|log| std::cmp::Reverse(log.created_at));

        let total = logs.len();
        let page_size = query.page_size;
        let total_pages = total.div_ceil(page_size);
        let page = query.page;
        let start = (page - 1) * page_size;
        let logs = if start >= total {
            Vec::new()
        } else {
            let end = (start + page_size).min(total);
            logs[start..end]
                .iter()
                .map(|log| enrich_usage_log(log))
                .collect()
        };

        Ok(UsageLogPage {
            logs,
            total,
            page,
            page_size,
            total_pages,
        })
    }

    pub async fn downstream_usage_summary(
        &self,
        downstream_id: &str,
    ) -> io::Result<DownstreamUsageSummary> {
        if let Some(summary) = self
            .config_store
            .downstream_usage_summary(downstream_id)
            .await?
        {
            return Ok(summary);
        }

        let snapshot = self.snapshot().await;
        let now = unix_seconds();
        build_downstream_usage_summary(&snapshot, downstream_id, now)
    }
}
