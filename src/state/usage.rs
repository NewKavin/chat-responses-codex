use super::types::*;
use crate::state::normalize_context_profile_base_url;
use crate::state::unix_seconds;
use crate::state::AppState;
use chrono::Datelike;
use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, Serialize)]
pub struct PerMinuteUsage {
    pub used: u32,
    pub limit: u32,
    pub percentage: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RequestQuotaUsage {
    pub used: u32,
    pub limit: u32,
    pub remaining: u32,
    pub window_hours: u32,
    pub percentage: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TokenUsage {
    pub daily: Option<TokenQuota>,
    pub monthly: Option<TokenQuota>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TokenQuota {
    pub used: u64,
    pub limit: u64,
    pub remaining: u64,
    pub percentage: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DailyStats {
    pub date: u64,
    pub total_requests: u32,
    pub total_tokens: u64,
    pub success_rate: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelStats {
    pub model: String,
    pub today_count: u32,
    pub month_count: u32,
    pub today_tokens: u64,
    pub month_tokens: u64,
    pub avg_latency_ms: u64,
    pub success_rate: f64,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct DownstreamTokenEvent {
    pub created_at: u64,
    pub tokens: u64,
}

pub(super) const DOWNSTREAM_DAILY_TOKEN_WINDOW_SECONDS: u64 = 24 * 60 * 60;
pub(super) const DOWNSTREAM_MONTHLY_TOKEN_WINDOW_SECONDS: u64 = 30 * 24 * 60 * 60;

pub(super) fn build_downstream_request_windows(
    logs: &[UsageLog],
) -> HashMap<String, VecDeque<u64>> {
    let mut windows = HashMap::new();
    for log in normalized_usage_logs(logs) {
        windows
            .entry(log.downstream_key_id.clone())
            .or_insert_with(VecDeque::new)
            .push_back(log.created_at);
    }
    windows
}

pub(super) fn build_downstream_token_windows(
    logs: &[UsageLog],
) -> HashMap<String, VecDeque<DownstreamTokenEvent>> {
    let mut windows = HashMap::new();
    for log in normalized_usage_logs(logs) {
        windows
            .entry(log.downstream_key_id.clone())
            .or_insert_with(VecDeque::new)
            .push_back(DownstreamTokenEvent {
                created_at: log.created_at,
                tokens: log.total_tokens,
            });
    }
    windows
}

pub(super) fn normalized_usage_logs(logs: &[UsageLog]) -> Vec<UsageLog> {
    let mut logs = logs.to_vec();
    logs.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then(left.request_id.cmp(&right.request_id))
            .then(left.id.cmp(&right.id))
    });

    let mut seen = HashSet::new();
    let mut deduped = Vec::with_capacity(logs.len());
    for log in logs {
        if seen.insert(log.id.clone()) {
            deduped.push(log);
        }
    }
    deduped
}

pub(super) fn downstream_token_retention_seconds(downstream: &DownstreamConfig) -> u64 {
    if downstream.monthly_token_limit.is_some() {
        DOWNSTREAM_MONTHLY_TOKEN_WINDOW_SECONDS
    } else if downstream.daily_token_limit.is_some() {
        DOWNSTREAM_DAILY_TOKEN_WINDOW_SECONDS
    } else {
        60
    }
}

pub(super) fn downstream_token_retry_after_seconds(
    events: &VecDeque<DownstreamTokenEvent>,
    now: u64,
    window_seconds: u64,
    deficit: u64,
) -> u64 {
    if deficit == 0 {
        return 1;
    }

    let mut freed = 0u64;
    for event in events {
        freed = freed.saturating_add(event.tokens);
        if freed >= deficit {
            return event
                .created_at
                .saturating_add(window_seconds)
                .saturating_sub(now)
                .max(1);
        }
    }

    window_seconds.max(1)
}

pub(super) fn build_active_upstream_model_catalog(
    snapshot: &PersistedState,
) -> HashMap<String, Vec<String>> {
    let mut catalog: HashMap<String, Vec<String>> = HashMap::new();
    let mut seen_exact = HashSet::new();

    for upstream in snapshot.upstreams.iter().filter(|upstream| upstream.active) {
        for model in upstream.route_models() {
            let model = model.trim();
            if model.is_empty() {
                continue;
            }

            let model = model.to_string();
            if !seen_exact.insert(model.clone()) {
                continue;
            }

            catalog
                .entry(model.to_ascii_lowercase())
                .or_default()
                .push(model);
        }
    }

    catalog
}

pub(super) fn canonicalize_portal_model_name(
    catalog: &HashMap<String, Vec<String>>,
    model: &str,
) -> Option<String> {
    let model = model.trim();
    if model.is_empty() {
        return None;
    }

    let lookup_key = model.to_ascii_lowercase();
    let Some(candidates) = catalog.get(&lookup_key) else {
        return Some(model.to_string());
    };
    if let Some(exact_match) = candidates
        .iter()
        .find(|candidate| candidate.as_str() == model)
    {
        return Some(exact_match.clone());
    }

    candidates
        .first()
        .cloned()
        .or_else(|| Some(model.to_string()))
}

pub(super) fn normalize_model_name(model: &str) -> Option<String> {
    let model = model.trim();
    if model.is_empty() {
        return None;
    }
    Some(model.to_ascii_lowercase())
}

pub fn portal_model_is_allowed(allowlist: &[String], model: &str) -> bool {
    if allowlist.is_empty() {
        return true;
    }

    let Some(model) = normalize_model_name(model) else {
        return false;
    };
    allowlist
        .iter()
        .filter_map(|allowed| normalize_model_name(allowed))
        .any(|allowed| allowed == model)
}

impl AppState {
    pub async fn compute_per_minute_usage(&self, downstream_id: &str) -> PerMinuteUsage {
        let now = unix_seconds();
        let one_minute_ago = now.saturating_sub(60);

        let snapshot = self.snapshot().await;

        let downstream = snapshot.downstreams.iter().find(|d| d.id == downstream_id);
        let limit = downstream
            .map(|d| {
                if d.rate_limit_enabled {
                    d.per_minute_limit
                } else {
                    0
                }
            })
            .unwrap_or(0);

        let used = snapshot
            .usage_logs
            .iter()
            .filter(|log| {
                log.downstream_key_id == downstream_id && log.created_at >= one_minute_ago
            })
            .count() as u32;

        let percentage = if limit > 0 {
            (used as f64 / limit as f64) * 100.0
        } else {
            0.0
        };

        PerMinuteUsage {
            used,
            limit,
            percentage,
        }
    }

    pub async fn compute_request_quota_usage(
        &self,
        downstream: &DownstreamConfig,
    ) -> Option<RequestQuotaUsage> {
        if !downstream.rate_limit_enabled || !downstream.uses_request_quota() {
            return None;
        }

        let window_hours = downstream.request_quota_window_hours.unwrap();
        let limit = downstream.request_quota_requests.unwrap();

        let now = unix_seconds();
        let window_start = now.saturating_sub((window_hours as u64) * 3600);

        let used_from_windows = {
            let windows = self.downstream_request_windows.lock().await;
            windows
                .get(&downstream.id)
                .map(|window| {
                    window
                        .iter()
                        .filter(|&&timestamp| timestamp >= window_start)
                        .count() as u32
                })
                .unwrap_or(0)
        };
        let used_from_logs = {
            let snapshot = self.snapshot().await;
            snapshot
                .usage_logs
                .iter()
                .filter(|log| {
                    log.downstream_key_id == downstream.id && log.created_at >= window_start
                })
                .count() as u32
        };
        let used = used_from_windows.max(used_from_logs);

        let percentage = if limit > 0 {
            (used as f64 / limit as f64) * 100.0
        } else {
            0.0
        };

        let remaining = limit.saturating_sub(used);

        Some(RequestQuotaUsage {
            used,
            limit,
            remaining,
            window_hours,
            percentage,
        })
    }

    pub async fn compute_token_usage(&self, downstream_id: &str, now: u64) -> TokenUsage {
        let snapshot = self.snapshot().await;

        let downstream = snapshot.downstreams.iter().find(|d| d.id == downstream_id);

        let daily_limit = downstream.and_then(|d| d.daily_token_limit);
        let monthly_limit = downstream.and_then(|d| d.monthly_token_limit);

        let today_start = (now / 86400) * 86400;

        let month_start = {
            use std::time::UNIX_EPOCH;
            let dt = UNIX_EPOCH + std::time::Duration::from_secs(now);
            let datetime = chrono::DateTime::<chrono::Utc>::from(dt);
            let first_of_month = datetime.date_naive().with_day(1).unwrap();
            first_of_month
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp() as u64
        };

        let daily = if let Some(limit) = daily_limit {
            let used: u64 = snapshot
                .usage_logs
                .iter()
                .filter(|log| {
                    log.downstream_key_id == downstream_id && log.created_at >= today_start
                })
                .map(|log| log.total_tokens)
                .sum();

            let percentage = if limit > 0 {
                (used as f64 / limit as f64) * 100.0
            } else {
                0.0
            };

            let remaining = limit.saturating_sub(used);

            Some(TokenQuota {
                used,
                limit,
                remaining,
                percentage,
            })
        } else {
            None
        };

        let monthly = if let Some(limit) = monthly_limit {
            let used: u64 = snapshot
                .usage_logs
                .iter()
                .filter(|log| {
                    log.downstream_key_id == downstream_id && log.created_at >= month_start
                })
                .map(|log| log.total_tokens)
                .sum();

            let percentage = if limit > 0 {
                (used as f64 / limit as f64) * 100.0
            } else {
                0.0
            };

            let remaining = limit.saturating_sub(used);

            Some(TokenQuota {
                used,
                limit,
                remaining,
                percentage,
            })
        } else {
            None
        };

        TokenUsage { daily, monthly }
    }

    pub async fn compute_daily_stats(&self, downstream_id: &str, days: usize) -> Vec<DailyStats> {
        let snapshot = self.snapshot().await;
        let now = unix_seconds();

        let mut stats = Vec::new();

        for day_offset in 0..days {
            let day_start = now.saturating_sub((day_offset as u64) * 86400);
            let day_start = (day_start / 86400) * 86400;
            let day_end = day_start + 86400;

            let day_logs: Vec<_> = snapshot
                .usage_logs
                .iter()
                .filter(|log| {
                    log.downstream_key_id == downstream_id
                        && log.created_at >= day_start
                        && log.created_at < day_end
                })
                .collect();

            let requests = day_logs.len() as u32;
            let tokens: u64 = day_logs.iter().map(|log| log.total_tokens).sum();

            let successful = day_logs.iter().filter(|log| log.status_code == 200).count();
            let success_rate = if requests > 0 {
                successful as f64 / requests as f64
            } else {
                0.0
            };

            stats.push(DailyStats {
                date: day_start,
                total_requests: requests,
                total_tokens: tokens,
                success_rate,
            });
        }

        stats
    }

    pub async fn compute_model_stats(&self, downstream: &DownstreamConfig) -> Vec<ModelStats> {
        let snapshot = self.snapshot().await;
        let now = unix_seconds();

        let today_start = (now / 86400) * 86400;
        let month_start = {
            use std::time::UNIX_EPOCH;
            let dt = UNIX_EPOCH + std::time::Duration::from_secs(now);
            let datetime = chrono::DateTime::<chrono::Utc>::from(dt);
            let first_of_month = datetime.date_naive().with_day(1).unwrap();
            first_of_month
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp() as u64
        };

        let canonical_models = build_active_upstream_model_catalog(&snapshot);

        let mut model_logs: std::collections::HashMap<String, Vec<&UsageLog>> =
            std::collections::HashMap::new();

        for log in &snapshot.usage_logs {
            if log.downstream_key_id != downstream.id {
                continue;
            }

            let Some(model) = canonicalize_portal_model_name(&canonical_models, &log.model) else {
                continue;
            };

            if !portal_model_is_allowed(&downstream.model_allowlist, &model) {
                continue;
            }

            model_logs.entry(model).or_default().push(log);
        }

        let mut stats = Vec::new();

        for (model, logs) in model_logs {
            let today_logs: Vec<&&UsageLog> = logs
                .iter()
                .filter(|log| log.created_at >= today_start)
                .collect();
            let month_logs: Vec<&&UsageLog> = logs
                .iter()
                .filter(|log| log.created_at >= month_start)
                .collect();

            let today_count = today_logs.len() as u32;
            let month_count = month_logs.len() as u32;
            let today_tokens: u64 = today_logs.iter().map(|log| log.total_tokens).sum();
            let month_tokens: u64 = month_logs.iter().map(|log| log.total_tokens).sum();

            let total_latency: u64 = logs.iter().map(|log| log.latency_ms).sum();
            let avg_latency_ms = if !logs.is_empty() {
                total_latency / logs.len() as u64
            } else {
                0
            };

            let successful = logs.iter().filter(|log| log.status_code == 200).count();
            let success_rate = if !logs.is_empty() {
                let raw_rate = successful as f64 / logs.len() as f64;
                (raw_rate * 100.0).round() / 100.0
            } else {
                0.0
            };

            stats.push(ModelStats {
                model,
                today_count,
                month_count,
                today_tokens,
                month_tokens,
                avg_latency_ms,
                success_rate,
            });
        }

        stats
    }

    pub async fn compute_portal_model_context_limits(
        &self,
        downstream: &DownstreamConfig,
    ) -> HashMap<String, ModelContextConfig> {
        let snapshot = self.snapshot().await;
        let canonical_models = build_active_upstream_model_catalog(&snapshot);

        let mut result: HashMap<String, ModelContextConfig> = HashMap::new();

        let allowlist: Vec<String> = if downstream.model_allowlist.is_empty() {
            canonical_models
                .values()
                .flat_map(|slugs| slugs.iter().cloned())
                .collect()
        } else {
            downstream
                .model_allowlist
                .iter()
                .map(|slug| slug.trim().to_string())
                .filter(|slug| !slug.is_empty())
                .collect()
        };

        for model in allowlist {
            if !portal_model_is_allowed(&downstream.model_allowlist, &model) {
                continue;
            }

            for upstream in snapshot.upstreams.iter().filter(|u| u.active) {
                let exposes = upstream
                    .route_models()
                    .iter()
                    .any(|candidate| candidate.trim().eq_ignore_ascii_case(model.trim()));
                if !exposes {
                    continue;
                }

                let base_url = normalize_context_profile_base_url(&upstream.base_url);
                let profile = if base_url.is_empty() {
                    None
                } else {
                    snapshot.global_context_profiles.get(&base_url)
                };

                let Some(cfg) = upstream.context_config_for_model_with_profile(&model, profile)
                else {
                    continue;
                };
                if cfg.context_limit == 0 {
                    continue;
                }

                result
                    .entry(model.clone())
                    .and_modify(|existing| {
                        if cfg.context_limit < existing.context_limit {
                            existing.context_limit = cfg.context_limit;
                            existing.output_reserve = cfg.output_reserve;
                        }
                    })
                    .or_insert(ModelContextConfig {
                        slug: model.clone(),
                        context_limit: cfg.context_limit,
                        output_reserve: cfg.output_reserve,
                        context_group: cfg.context_group.clone(),
                    });
            }
        }

        result
    }
}
