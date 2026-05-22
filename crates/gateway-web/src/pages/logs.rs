use leptos::prelude::*;

use gateway_core::state::{unix_seconds, AppConfig, PersistedState, UsageLog};
use serde::{Deserialize, Serialize};

use crate::shell::{AppLayout, Panel, Section, StatCard, Tone};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct LogListQuery {
    request_id: Option<String>,
    downstream: Option<String>,
    upstream: Option<String>,
    endpoint: Option<String>,
    status: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LogStatusFilter {
    All,
    Success,
    Warning,
}

impl LogListQuery {
    fn request_id_value(&self) -> String {
        normalized_text(&self.request_id)
    }

    fn downstream_value(&self) -> String {
        normalized_text(&self.downstream)
    }

    fn upstream_value(&self) -> String {
        normalized_text(&self.upstream)
    }

    fn endpoint_value(&self) -> String {
        normalized_text(&self.endpoint)
    }

    #[cfg(test)]
    fn normalized(&self) -> Self {
        Self {
            request_id: normalized_option_text(&self.request_id),
            downstream: normalized_option_text(&self.downstream),
            upstream: normalized_option_text(&self.upstream),
            endpoint: normalized_option_text(&self.endpoint),
            status: match self.status_filter() {
                LogStatusFilter::Success => Some("success".to_string()),
                LogStatusFilter::Warning => Some("warning".to_string()),
                LogStatusFilter::All => None,
            },
        }
    }

    #[cfg(test)]
    fn query_suffix(&self) -> String {
        let encoded = serde_urlencoded::to_string(&self.normalized()).unwrap_or_default();
        if encoded.is_empty() {
            String::new()
        } else {
            format!("?{encoded}")
        }
    }

    fn status_filter(&self) -> LogStatusFilter {
        match self.status.as_deref().map(str::trim) {
            Some(value) if value.eq_ignore_ascii_case("success") => LogStatusFilter::Success,
            Some(value) if value.eq_ignore_ascii_case("warning") => LogStatusFilter::Warning,
            _ => LogStatusFilter::All,
        }
    }

    fn matches(&self, state: &PersistedState, log: &UsageLog) -> bool {
        let request_id = self.request_id_value();
        if !contains_filter(&log.request_id, &request_id) {
            return false;
        }

        let downstream = self.downstream_value();
        if !downstream.is_empty() {
            let downstream_name = resolve_downstream_name(state, &log.downstream_key_id);
            if !contains_filter(&downstream_name, &downstream)
                && !contains_filter(&log.downstream_key_id, &downstream)
            {
                return false;
            }
        }

        let upstream = self.upstream_value();
        if !upstream.is_empty() {
            let upstream_name = resolve_upstream_name(state, &log.upstream_key_id);
            if !contains_filter(&upstream_name, &upstream)
                && !contains_filter(&log.upstream_key_id, &upstream)
            {
                return false;
            }
        }

        let endpoint = self.endpoint_value();
        if !contains_filter(&log.endpoint, &endpoint) {
            return false;
        }

        match self.status_filter() {
            LogStatusFilter::All => {}
            LogStatusFilter::Success if !matches!(log.status_code, 200..=299) => return false,
            LogStatusFilter::Warning if log.status_code < 400 => return false,
            LogStatusFilter::Success | LogStatusFilter::Warning => {}
        }

        true
    }
}

#[component]
pub fn LogsPage(config: AppConfig, state: PersistedState, query: LogListQuery) -> impl IntoView {
    let app_name = config.app_name.clone();
    let filtered_logs = filtered_usage_logs(&state, &query);
    let summary = usage_summary(&filtered_logs);
    let rows = usage_rows(&state, &filtered_logs, 8);
    let recent_excerpt = recent_log_excerpt(&rows);
    let request_id_value = query.request_id_value();
    let downstream_value = query.downstream_value();
    let upstream_value = query.upstream_value();
    let endpoint_value = query.endpoint_value();
    let status_filter = query.status_filter();
    let status_all_selected = matches!(status_filter, LogStatusFilter::All);
    let status_success_selected = matches!(status_filter, LogStatusFilter::Success);
    let status_warning_selected = matches!(status_filter, LogStatusFilter::Warning);

    view! {
        <AppLayout
            title="运行日志"
            subtitle=format!("{app_name} 的请求、降级和鉴权事件。")
            active=Section::Logs
        >
            <section class="summary-grid">
                <StatCard
                    label="总请求"
                    value=summary.total.to_string()
                    hint="共享 state 中记录的调用次数"
                    tone=Tone::Teal
                />
                <StatCard
                    label="成功请求"
                    value=summary.success.to_string()
                    hint=format!("{}% 的请求成功返回", summary.success_rate)
                    tone=Tone::Blue
                />
                <StatCard
                    label="告警请求"
                    value=summary.warnings.to_string()
                    hint="429、5xx 和 fallback 事件"
                    tone=Tone::Gold
                />
                <StatCard
                    label="平均耗时"
                    value=format!("{}ms", summary.avg_latency_ms)
                    hint="按最近日志粗略计算"
                    tone=Tone::Rose
                />
            </section>

            <div class="note">
                <strong>提示</strong>
                <span>{"Token 数据仅供参考，不影响限额判断。"}</span>
            </div>

            <Panel
                title="日志列表"
                subtitle=format!("结构化日志直接来自共享 state。当前匹配 {} 条记录。", filtered_logs.len())
            >
                <div class="section-stack">
                  <form class="form-grid panel-toolbar" method="get" action="/admin/logs" data-log-filter="true">
                    <div class="field">
                      <label for="request_id">请求 ID</label>
                      <input id="request_id" name="request_id" value=request_id_value placeholder="REQ-1041" />
                    </div>
                    <div class="field">
                      <label for="downstream">下游</label>
                      <input id="downstream" name="downstream" value=downstream_value placeholder="Team A" />
                    </div>
                    <div class="field">
                      <label for="upstream">上游</label>
                      <input id="upstream" name="upstream" value=upstream_value placeholder="GLM 主账号" />
                    </div>
                    <div class="field">
                      <label for="endpoint">路径</label>
                      <input id="endpoint" name="endpoint" value=endpoint_value placeholder="/v1/responses" />
                    </div>
                    <div class="field">
                      <label for="status">状态</label>
                      <select id="status" name="status">
                        <option value="" selected=status_all_selected>全部</option>
                        <option value="success" selected=status_success_selected>成功</option>
                        <option value="warning" selected=status_warning_selected>告警</option>
                      </select>
                    </div>
                    <div class="actions">
                      <button class="button primary" type="submit">应用筛选</button>
                      <a class="button secondary" href="/admin/logs">重置筛选</a>
                    </div>
                  </form>
                  <div class="table-shell">
                    <table class="table">
                      <thead>
                        <tr>
                          <th>时间</th>
                          <th>请求 ID</th>
                          <th>下游</th>
                          <th>上游</th>
                          <th>模型</th>
                          <th>路径</th>
                          <th>状态</th>
                          <th>Token 吞吐</th>
                          <th>耗时</th>
                        </tr>
                      </thead>
                      <tbody>
                        {rows
                            .into_iter()
                            .map(|row| view! {
                              <tr>
                                  <td>{row.age_label}</td>
                                  <td><strong>{row.request_id}</strong></td>
                                  <td>
                                    <strong>{row.downstream_name}</strong>
                                  </td>
                                  <td>
                                    <strong>{row.upstream_name}</strong>
                                  </td>
                                  <td>{row.model}</td>
                                  <td>{row.endpoint}</td>
                                  <td><span class=row.status_class>{row.status_label}</span></td>
                                  <td>
                                    <strong>{row.throughput}</strong>
                                    <div class="hint">{row.token_breakdown}</div>
                                  </td>
                                  <td>{row.latency}</td>
                                </tr>
                            })
                            .collect::<Vec<_>>()}
                      </tbody>
                    </table>
                  </div>
                </div>
            </Panel>

            <Panel title="事件摘要" subtitle="最近的请求形态和异常比例一眼可见。">
                <div class="section-stack">
                  <div class="note">
                    {format!(
                        "Responses 路径 {} 条，Chat 路径 {} 条，失败或告警 {} 条。",
                        summary.responses_paths, summary.chat_paths, summary.warnings
                    )}
                  </div>
                  <div class="code-block">{recent_excerpt}</div>
                </div>
            </Panel>
        </AppLayout>
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct UsageSummary {
    total: usize,
    success: usize,
    warnings: usize,
    responses_paths: usize,
    chat_paths: usize,
    avg_latency_ms: u64,
    success_rate: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct UsageRow {
    age_label: String,
    request_id: String,
    downstream_name: String,
    upstream_name: String,
    model: String,
    endpoint: String,
    status_label: String,
    status_class: String,
    throughput: String,
    token_breakdown: String,
    latency: String,
}

fn usage_summary(logs: &[UsageLog]) -> UsageSummary {
    let total = logs.len();
    let success = logs
        .iter()
        .filter(|log| (200..300).contains(&log.status_code))
        .count();
    let warnings = logs.iter().filter(|log| log.status_code >= 400).count();
    let responses_paths = logs
        .iter()
        .filter(|log| log.endpoint == "/v1/responses")
        .count();
    let chat_paths = logs
        .iter()
        .filter(|log| log.endpoint == "/v1/chat/completions")
        .count();
    let latency_sum = logs.iter().map(|log| log.latency_ms).sum::<u64>();
    let avg_latency_ms = if total == 0 {
        0
    } else {
        latency_sum / total as u64
    };
    let success_rate = if total == 0 {
        0
    } else {
        (success as u64 * 100) / total as u64
    };

    UsageSummary {
        total,
        success,
        warnings,
        responses_paths,
        chat_paths,
        avg_latency_ms,
        success_rate,
    }
}

fn usage_rows(state: &PersistedState, logs: &[UsageLog], limit: usize) -> Vec<UsageRow> {
    let now = unix_seconds();

    logs.iter()
        .take(limit)
        .map(|log| UsageRow {
            age_label: age_label(now.saturating_sub(log.created_at)),
            request_id: log.request_id.clone(),
            downstream_name: resolve_downstream_name(state, &log.downstream_key_id),
            upstream_name: resolve_upstream_name(state, &log.upstream_key_id),
            model: log.model.clone(),
            endpoint: log.endpoint.clone(),
            status_label: status_label(log.status_code),
            status_class: status_class(log.status_code).to_string(),
            throughput: throughput_label(log.total_tokens, log.latency_ms),
            token_breakdown: token_breakdown_label(log),
            latency: format!("{}ms", log.latency_ms),
        })
        .collect()
}

fn filtered_usage_logs(state: &PersistedState, query: &LogListQuery) -> Vec<UsageLog> {
    state
        .usage_logs
        .iter()
        .rev()
        .filter(|log| query.matches(state, log))
        .cloned()
        .collect()
}

fn normalized_text(value: &Option<String>) -> String {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
fn normalized_option_text(value: &Option<String>) -> Option<String> {
    let value = normalized_text(value);
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn contains_filter(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }

    haystack.to_lowercase().contains(&needle.to_lowercase())
}

fn throughput_label(total_tokens: u64, latency_ms: u64) -> String {
    let latency = latency_ms.max(1) as u128;
    let throughput = (total_tokens as u128 * 1_000) / latency;
    format!("{throughput} tok/s")
}

fn token_breakdown_label(log: &UsageLog) -> String {
    format!(
        "{} / {} / {} tokens",
        log.prompt_tokens, log.completion_tokens, log.total_tokens
    )
}

fn resolve_downstream_name(state: &PersistedState, downstream_key_id: &str) -> String {
    state
        .downstreams
        .iter()
        .find(|downstream| downstream.id == downstream_key_id)
        .map(|downstream| downstream.name.clone())
        .unwrap_or_else(|| downstream_key_id.to_string())
}

fn resolve_upstream_name(state: &PersistedState, upstream_key_id: &str) -> String {
    state
        .upstreams
        .iter()
        .find(|upstream| upstream.id == upstream_key_id)
        .map(|upstream| upstream.name.clone())
        .unwrap_or_else(|| upstream_key_id.to_string())
}

fn status_label(status_code: u16) -> String {
    match status_code {
        200..=299 => format!("{status_code} OK"),
        300..=399 => format!("{status_code} Redirect"),
        400..=499 => format!("{status_code} Client"),
        _ => format!("{status_code} Upstream"),
    }
}

fn status_class(status_code: u16) -> &'static str {
    match status_code {
        200..=299 => "badge badge-success",
        300..=399 => "badge badge-info",
        400..=499 => "badge badge-warning",
        _ => "badge badge-strong",
    }
}

fn age_label(age_seconds: u64) -> String {
    match age_seconds {
        0..=59 => "刚刚".to_string(),
        60..=3_599 => format!("{} 分钟前", age_seconds / 60),
        3_600..=86_399 => format!("{} 小时前", age_seconds / 3_600),
        _ => format!("{} 天前", age_seconds / 86_400),
    }
}

fn recent_log_excerpt(rows: &[UsageRow]) -> String {
    if rows.is_empty() {
        return "暂无日志".to_string();
    }

    rows.iter()
        .take(3)
        .map(|row| {
            format!(
                "{} {} {} {} {} {}",
                row.age_label,
                row.request_id,
                row.endpoint,
                row.status_label,
                row.model,
                row.throughput
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::render_logs_page;
    use gateway_core::routing::UpstreamProtocol;
    use gateway_core::state::{
        DownstreamConfig, ModelAliasConfig, ModelRequestCostConfig, UpstreamConfig, UsageLog,
    };

    fn sample_state() -> PersistedState {
        PersistedState {
            upstreams: vec![UpstreamConfig {
                id: "up-1".into(),
                name: "Primary".into(),
                base_url: "https://example.com".into(),
                api_key: "sk-demo".into(),
                protocol: UpstreamProtocol::Responses,
                supported_models: vec!["glm-5".into()],
                model_aliases: vec![ModelAliasConfig {
                    slug: "glm-5".into(),
                    upstream_model: "GLM-5".into(),
                }],
                request_quota_5h: 600,
                requests_per_minute: 20,
                max_concurrency: 4,
                model_request_costs: vec![ModelRequestCostConfig {
                    slug: "glm-5".into(),
                    cost: 2,
                }],
                active: true,
                failure_count: 0,
            }],
            downstreams: vec![DownstreamConfig {
                id: "down-1".into(),
                name: "Team".into(),
                hash: "sha256:demo".into(),
                plaintext_key: Some("sk-demo".into()),
                model_allowlist: vec!["glm-5".into()],
                per_minute_limit: 20,
                daily_token_limit: Some(1_000),
                monthly_token_limit: Some(2_000),
                request_quota_window_hours: None,
                request_quota_requests: None,
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }],
            usage_logs: vec![
                UsageLog {
                    id: "log-1".into(),
                    downstream_key_id: "down-1".into(),
                    upstream_key_id: "up-1".into(),
                    endpoint: "/v1/responses".into(),
                    model: "glm-5".into(),
                    request_id: "REQ-1".into(),
                    status_code: 200,
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    total_tokens: 15,
                    latency_ms: 12,
                    created_at: unix_seconds().saturating_sub(61),
                },
                UsageLog {
                    id: "log-2".into(),
                    downstream_key_id: "down-1".into(),
                    upstream_key_id: "up-1".into(),
                    endpoint: "/v1/chat/completions".into(),
                    model: "glm-5".into(),
                    request_id: "REQ-2".into(),
                    status_code: 502,
                    prompt_tokens: 20,
                    completion_tokens: 0,
                    total_tokens: 20,
                    latency_ms: 24,
                    created_at: unix_seconds().saturating_sub(3_700),
                },
            ],
        }
    }

    #[test]
    fn summary_counts_usage_by_status_and_route() {
        let state = sample_state();
        let summary = usage_summary(&state.usage_logs);
        assert_eq!(summary.total, 2);
        assert_eq!(summary.success, 1);
        assert_eq!(summary.warnings, 1);
        assert_eq!(summary.responses_paths, 1);
        assert_eq!(summary.chat_paths, 1);
        assert_eq!(summary.avg_latency_ms, 18);
        assert_eq!(summary.success_rate, 50);
    }

    #[test]
    fn rows_render_newest_first() {
        let state = sample_state();
        let logs = filtered_usage_logs(&state, &LogListQuery::default());
        let rows = usage_rows(&state, &logs, 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].request_id, "REQ-2");
        assert_eq!(rows[0].status_label, "502 Upstream");
    }

    #[test]
    fn rendered_page_shows_resolved_names_tokens_and_filters() {
        let html = render_logs_page().0;

        assert!(html.contains("Team A"));
        assert!(html.contains("GLM 主账号"));
        assert!(html.contains("Token 数据仅供参考，不影响限额判断"));
        assert!(html.contains("Token 吞吐"));
        assert!(html.contains("tok/s"));
        assert!(!html.contains("down-1"));
        assert!(!html.contains("up-1"));
        assert!(html.contains(r#"data-log-filter="true""#));
        assert!(html.contains(r#"class="form-grid panel-toolbar""#));
        assert!(html.contains(r#"name="request_id""#));
        assert!(html.contains(r#"name="downstream""#));
        assert!(html.contains(r#"name="upstream""#));
        assert!(html.contains(r#"name="endpoint""#));
        assert!(html.contains(r#"name="status""#));
    }

    #[test]
    fn rendered_page_applies_query_filters() {
        let query = LogListQuery {
            request_id: Some("REQ-1043".into()),
            downstream: Some("Team A".into()),
            upstream: Some("GLM 主账号".into()),
            endpoint: Some("/v1/responses".into()),
            status: Some("warning".into()),
        };

        assert!(query.query_suffix().contains("request_id=REQ-1043"));

        let html = crate::app::render_logs_page_with_query(query).0;
        assert!(html.contains("REQ-1043"));
        assert!(html.contains("Team A"));
        assert!(html.contains("tok/s"));
        assert!(!html.contains("REQ-1042"));
        assert!(!html.contains("/v1/chat/completions"));
        assert!(!html.contains("Legacy Lab"));
    }
}
