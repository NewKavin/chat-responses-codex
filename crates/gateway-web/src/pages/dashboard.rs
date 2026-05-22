use leptos::prelude::*;

use gateway_core::state::{AppConfig, PersistedState};

use crate::shell::{AppLayout, Panel, Section, StatCard, Tone};

#[component]
pub fn DashboardPage(config: AppConfig, state: PersistedState) -> impl IntoView {
    let app_name = config.app_name.clone();
    let stats = dashboard_stats(&config, &state);
    let recent_events = recent_usage_rows(&state, 4);

    view! {
        <AppLayout
            title="仪表盘"
            subtitle=format!("{app_name} 的上游、下游和请求日志概览。")
            active=Section::Dashboard
        >
            <section class="summary-grid">
                {stats
                    .into_iter()
                    .map(|stat| view! {
                        <StatCard label=stat.label value=stat.value hint=stat.hint tone=stat.tone />
                    })
                    .collect::<Vec<_>>()}
            </section>

            <Panel title="最近请求" subtitle="展示最近的协议转换、降级和流式行为。">
                <div class="section-stack">
                  <div class="note">{"这些日志直接来自共享的 PersistedState，所以页面展示的就是核心层真实记录，而不是前端自造数据。"}</div>
                  <div class="table-shell">
                    <table class="table">
                      <thead>
                        <tr>
                          <th>请求 ID</th>
                          <th>下游</th>
                          <th>上游</th>
                          <th>模型</th>
                          <th>路径</th>
                          <th>结果</th>
                          <th>耗时</th>
                        </tr>
                      </thead>
                      <tbody>
                        {recent_events
                            .into_iter()
                            .map(|event| view! {
                                <tr>
                                  <td><strong>{event.request_id}</strong></td>
                                  <td>{event.downstream_key_id}</td>
                                  <td>{event.upstream_key_id}</td>
                                  <td>{event.model}</td>
                                  <td>{event.endpoint}</td>
                                  <td><span class=event.status_class>{event.status_label}</span></td>
                                  <td>{event.latency}</td>
                                </tr>
                            })
                            .collect::<Vec<_>>()}
                      </tbody>
                    </table>
                  </div>
                </div>
            </Panel>

            <Panel title="运行状态" subtitle="当前视图只负责展示，不参与网关决策。">
                <div class="section-stack">
                  <div class="note">{"协议转换、上游选择、限流和 fallback 仍然留在后端核心；Leptos 只承担管理后台表现层。"}</div>
                  <div class="empty-state">{"这里后续可以接健康检查、告警摘要和最近的降级趋势图。"}</div>
                </div>
            </Panel>
        </AppLayout>
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DashboardStat {
    value: String,
    label: String,
    hint: String,
    tone: Tone,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct UsageRow {
    request_id: String,
    downstream_key_id: String,
    upstream_key_id: String,
    model: String,
    endpoint: String,
    status_label: String,
    status_class: String,
    latency: String,
}

fn dashboard_stats(config: &AppConfig, state: &PersistedState) -> Vec<DashboardStat> {
    let upstream_total = state.upstreams.len();
    let active_upstreams = state
        .upstreams
        .iter()
        .filter(|upstream| upstream.active)
        .count();
    let downstream_total = state.downstreams.len();
    let active_downstreams = state
        .downstreams
        .iter()
        .filter(|downstream| downstream.active)
        .count();
    let request_logs = state.usage_logs.len();
    let failure_count = state
        .upstreams
        .iter()
        .map(|upstream| upstream.failure_count)
        .sum::<u32>();

    vec![
        DashboardStat {
            value: format!("{active_upstreams}/{upstream_total}"),
            label: "启用上游".to_string(),
            hint: format!("{} 的可用路由池", config.app_name),
            tone: Tone::Teal,
        },
        DashboardStat {
            value: format!("{active_downstreams}/{downstream_total}"),
            label: "下游密钥".to_string(),
            hint: "当前可用的客户端会话".to_string(),
            tone: Tone::Blue,
        },
        DashboardStat {
            value: request_logs.to_string(),
            label: "结构化日志".to_string(),
            hint: "来自共享 state 的调用记录".to_string(),
            tone: Tone::Gold,
        },
        DashboardStat {
            value: failure_count.to_string(),
            label: "失败计数".to_string(),
            hint: "需要观察的上游异常".to_string(),
            tone: Tone::Rose,
        },
    ]
}

fn recent_usage_rows(state: &PersistedState, limit: usize) -> Vec<UsageRow> {
    state
        .usage_logs
        .iter()
        .rev()
        .take(limit)
        .map(|log| UsageRow {
            request_id: log.request_id.clone(),
            downstream_key_id: log.downstream_key_id.clone(),
            upstream_key_id: log.upstream_key_id.clone(),
            model: log.model.clone(),
            endpoint: log.endpoint.clone(),
            status_label: usage_status_label(log.status_code),
            status_class: usage_status_class(log.status_code).to_string(),
            latency: format!("{}ms", log.latency_ms),
        })
        .collect()
}

fn usage_status_label(status_code: u16) -> String {
    match status_code {
        200..=299 => format!("{status_code} OK"),
        300..=399 => format!("{status_code} Redirect"),
        400..=499 => format!("{status_code} Client"),
        _ => format!("{status_code} Upstream"),
    }
}

fn usage_status_class(status_code: u16) -> &'static str {
    match status_code {
        200..=299 => "badge badge-success",
        300..=399 => "badge badge-info",
        400..=499 => "badge badge-warning",
        _ => "badge badge-strong",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gateway_core::routing::UpstreamProtocol;
    use gateway_core::state::{
        DownstreamConfig, ModelAliasConfig, ModelRequestCostConfig, UpstreamConfig, UsageLog,
    };

    fn sample_upstream(active: bool, failure_count: u32) -> UpstreamConfig {
        UpstreamConfig {
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
            active,
            failure_count,
        }
    }

    fn sample_downstream(active: bool) -> DownstreamConfig {
        DownstreamConfig {
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
            active,
        }
    }

    fn sample_log(request_id: &str, status_code: u16, created_at: u64) -> UsageLog {
        UsageLog {
            id: request_id.to_string(),
            downstream_key_id: "down-1".into(),
            upstream_key_id: "up-1".into(),
            endpoint: "/v1/responses".into(),
            model: "glm-5".into(),
            request_id: request_id.to_string(),
            status_code,
            prompt_tokens: 1,
            completion_tokens: 1,
            total_tokens: 2,
            latency_ms: 12,
            created_at,
        }
    }

    #[test]
    fn dashboard_stats_are_derived_from_state() {
        let config = AppConfig::default();
        let state = PersistedState {
            upstreams: vec![sample_upstream(true, 2), sample_upstream(false, 1)],
            downstreams: vec![sample_downstream(true), sample_downstream(false)],
            usage_logs: vec![sample_log("REQ-1", 200, 10), sample_log("REQ-2", 502, 20)],
        };

        let stats = dashboard_stats(&config, &state);
        assert_eq!(stats[0].value, "1/2");
        assert_eq!(stats[1].value, "1/2");
        assert_eq!(stats[2].value, "2");
        assert_eq!(stats[3].value, "3");
    }

    #[test]
    fn recent_usage_rows_are_newest_first() {
        let state = PersistedState {
            upstreams: vec![],
            downstreams: vec![],
            usage_logs: vec![sample_log("REQ-1", 200, 10), sample_log("REQ-2", 502, 20)],
        };

        let rows = recent_usage_rows(&state, 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].request_id, "REQ-2");
        assert_eq!(rows[0].status_label, "502 Upstream");
        assert_eq!(rows[0].status_class, "badge badge-strong");
    }
}
