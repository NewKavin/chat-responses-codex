use leptos::prelude::*;

use gateway_core::routing::UpstreamProtocol;
use gateway_core::state::{AppConfig, ModelAliasConfig, ModelRequestCostConfig, PersistedState};

use crate::shell::{AppLayout, DisclosurePanel, Panel, Section, StatCard, Tone};

#[component]
pub fn UpstreamsPage(
    config: AppConfig,
    state: PersistedState,
    form: gateway_core::admin::UpstreamFormView,
    notice: String,
    form_open: bool,
) -> impl IntoView {
    let app_name = config.app_name.clone();
    let summary = upstream_summary(&state);
    let rows = upstream_rows(&state);

    let heading = form.heading.clone();
    let action = form.action.clone();
    let submit_label = form.submit_label.clone();
    let name = form.name.clone();
    let base_url = form.base_url.clone();
    let api_key = form.api_key.clone();
    let models = form.models.clone();
    let model_aliases = form.model_aliases.clone();
    let request_quota_5h = form.request_quota_5h.clone();
    let requests_per_minute = form.requests_per_minute.clone();
    let max_concurrency = form.max_concurrency.clone();
    let model_request_costs = form.model_request_costs.clone();
    let active = form.active;
    let protocol = form.protocol;

    view! {
        <AppLayout
            title="上游密钥"
            subtitle=format!("{app_name} 的上游池、模型映射和请求限额。")
            active=Section::Upstreams
        >
            <section class="summary-grid">
                <StatCard
                    label="启用上游"
                    value=format!("{}/{}", summary.active, summary.total)
                    hint="参与路由的上游账号数量"
                    tone=Tone::Teal
                />
                <StatCard
                    label="Responses 池"
                    value=summary.responses.to_string()
                    hint="原生 Responses 协议上游"
                    tone=Tone::Blue
                />
                <StatCard
                    label="模型别名"
                    value=summary.alias_rules.to_string()
                    hint="大小写或上游名称映射"
                    tone=Tone::Gold
                />
                <StatCard
                    label="计费规则"
                    value=summary.cost_rules.to_string()
                    hint="模型请求次数的成本配置"
                    tone=Tone::Rose
                />
            </section>

            <DisclosurePanel
                title="上游表单"
                subtitle="点击右上角按钮展开新增或编辑上游。"
                action_label=heading
                open=form_open
            >
                <div class="section-stack">
                  <div class="note">{notice}</div>
                  <form class="section-stack" method="post" action=action>
                    <div class="form-grid">
                      <div class="field">
                        <label for="upstream-name">名称</label>
                        <input id="upstream-name" name="name" value=name />
                      </div>
                      <div class="field">
                        <label for="upstream-base-url">Base URL</label>
                        <input id="upstream-base-url" name="base_url" value=base_url />
                      </div>
                      <div class="field">
                        <label for="upstream-api-key">API Key</label>
                        <input id="upstream-api-key" name="api_key" type="password" value=api_key />
                      </div>
                      <div class="field">
                        <label for="upstream-protocol">协议</label>
                        <select id="upstream-protocol" name="protocol">
                          <option selected=move || matches!(protocol, UpstreamProtocol::ChatCompletions)>
                            ChatCompletions
                          </option>
                          <option selected=move || matches!(protocol, UpstreamProtocol::Responses)>
                            Responses
                          </option>
                        </select>
                        <span class="hint">{"按上游实际能力选择，网关会在后端做协议转换。"}</span>
                      </div>
                      <div class="field">
                        <label for="request-quota">5小时请求上限</label>
                        <input id="request-quota" name="request_quota_5h" type="number" value=request_quota_5h />
                      </div>
                      <div class="field">
                        <label for="requests-per-minute">每分钟请求上限</label>
                        <input id="requests-per-minute" name="requests_per_minute" type="number" value=requests_per_minute />
                      </div>
                      <div class="field">
                        <label for="max-concurrency">最大并发</label>
                        <input id="max-concurrency" name="max_concurrency" type="number" value=max_concurrency />
                      </div>
                      <div class="field">
                        <label for="upstream-active">启用状态</label>
                        <input id="upstream-active" name="active" type="checkbox" checked=active />
                        <span class="hint">{"关闭后配置仍保留，只是不再参与路由。"}</span>
                      </div>
                      <div class="field">
                        <label for="models">模型列表</label>
                        <textarea id="models" name="models" placeholder="glm-5&#10;glm-5.1">{models}</textarea>
                        <span class="hint">{"上游抓取模型后会先归一成小写，再把真正需要保留大小写的项写入别名。"}</span>
                      </div>
                      <div class="field">
                        <label for="model-aliases">模型别名</label>
                        <textarea
                          id="model-aliases"
                          name="model_aliases"
                          placeholder="glm-5=GLM-5"
                        >{model_aliases}</textarea>
                        <span class="hint">{"按 slug=UpstreamModel 的形式填写。"}</span>
                      </div>
                      <div class="field">
                        <label for="model-request-costs">模型请求成本</label>
                        <textarea
                          id="model-request-costs"
                          name="model_request_costs"
                          placeholder="glm-5=2&#10;glm-5.1=2"
                        >{model_request_costs}</textarea>
                        <span class="hint">{"支持把 GLM-5 / GLM-5.1 这类高成本模型单独计次。"}</span>
                      </div>
                    </div>
                    <div class="actions">
                      <button class="button primary" type="submit">{submit_label}</button>
                    </div>
                  </form>
                </div>
            </DisclosurePanel>

            <Panel title="上游列表" subtitle="展示协议、模型映射、计费规则和运行状态。">
                <div class="table-shell">
                  <table class="table">
                    <thead>
                      <tr>
                        <th>名称</th>
                        <th>Base URL</th>
                        <th>协议</th>
                        <th>模型 / 别名</th>
                        <th>限额 / 成本</th>
                        <th>状态</th>
                        <th>操作</th>
                      </tr>
                    </thead>
                    <tbody>
                      {rows
                          .into_iter()
                          .map(|row| view! {
                              <tr>
                                <td>
                                  <strong>{row.name}</strong>
                                  <div class="hint">{row.failure_hint}</div>
                                </td>
                                <td>{row.base_url}</td>
                                <td><span class="badge badge-muted">{row.protocol}</span></td>
                                <td>
                                  <strong>{row.models}</strong>
                                  <div class="hint">{row.alias_summary}</div>
                                </td>
                                <td>
                                  <strong>{row.limit_summary}</strong>
                                  <div class="hint">{row.cost_summary}</div>
                                </td>
                                <td><span class=row.status_class>{row.status_label}</span></td>
                                <td>
                                  <a class="button ghost" href=row.edit_href>编辑</a>
                                </td>
                              </tr>
                          })
                          .collect::<Vec<_>>()}
                    </tbody>
                  </table>
                </div>
            </Panel>
        </AppLayout>
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct UpstreamSummary {
    total: usize,
    active: usize,
    responses: usize,
    alias_rules: usize,
    cost_rules: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct UpstreamRow {
    name: String,
    base_url: String,
    protocol: String,
    models: String,
    alias_summary: String,
    cost_summary: String,
    limit_summary: String,
    status_label: String,
    status_class: String,
    failure_hint: String,
    edit_href: String,
}

fn upstream_summary(state: &PersistedState) -> UpstreamSummary {
    let total = state.upstreams.len();
    let active = state
        .upstreams
        .iter()
        .filter(|upstream| upstream.active)
        .count();
    let responses = state
        .upstreams
        .iter()
        .filter(|upstream| upstream.protocol == UpstreamProtocol::Responses)
        .count();
    let alias_rules = state
        .upstreams
        .iter()
        .map(|upstream| upstream.model_aliases.len())
        .sum();
    let cost_rules = state
        .upstreams
        .iter()
        .map(|upstream| upstream.model_request_costs.len())
        .sum();

    UpstreamSummary {
        total,
        active,
        responses,
        alias_rules,
        cost_rules,
    }
}

fn upstream_rows(state: &PersistedState) -> Vec<UpstreamRow> {
    state
        .upstreams
        .iter()
        .map(|upstream| UpstreamRow {
            name: upstream.name.clone(),
            base_url: upstream.base_url.clone(),
            protocol: protocol_label(upstream.protocol).to_string(),
            models: upstream.route_models().join(", "),
            alias_summary: format_model_aliases(&upstream.model_aliases),
            cost_summary: format_model_request_costs(&upstream.model_request_costs),
            limit_summary: format!(
                "5h {} · /min {} · 并发 {}",
                upstream.request_quota_5h, upstream.requests_per_minute, upstream.max_concurrency
            ),
            status_label: if upstream.active {
                format!("启用 · {} 次失败", upstream.failure_count)
            } else {
                format!("停用 · {} 次失败", upstream.failure_count)
            },
            status_class: if upstream.active {
                "badge badge-success".to_string()
            } else {
                "badge badge-warning".to_string()
            },
            failure_hint: if upstream.failure_count == 0 {
                "最近没有失败".to_string()
            } else {
                format!("最近累计 {} 次失败", upstream.failure_count)
            },
            edit_href: upstream_edit_href(&upstream.id),
        })
        .collect()
}

fn upstream_edit_href(upstream_id: &str) -> String {
    format!("/admin/upstreams?edit={upstream_id}")
}

fn protocol_label(protocol: UpstreamProtocol) -> &'static str {
    match protocol {
        UpstreamProtocol::ChatCompletions => "ChatCompletions",
        UpstreamProtocol::Responses => "Responses",
    }
}

fn format_model_aliases(aliases: &[ModelAliasConfig]) -> String {
    if aliases.is_empty() {
        return "别名：无".to_string();
    }

    format!(
        "别名：{}",
        aliases
            .iter()
            .map(|alias| format!("{}={}", alias.slug, alias.upstream_model))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn format_model_request_costs(costs: &[ModelRequestCostConfig]) -> String {
    if costs.is_empty() {
        return "计费：默认 1".to_string();
    }

    format!(
        "计费：{}",
        costs
            .iter()
            .map(|rule| format!("{}={}", rule.slug, rule.cost))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gateway_core::routing::UpstreamProtocol;
    use gateway_core::state::{DownstreamConfig, UpstreamConfig};

    fn sample_upstream(
        active: bool,
        failure_count: u32,
        protocol: UpstreamProtocol,
    ) -> UpstreamConfig {
        UpstreamConfig {
            id: "up-1".into(),
            name: "Primary".into(),
            base_url: "https://example.com".into(),
            api_key: "sk-demo".into(),
            protocol,
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

    #[test]
    fn upstream_summary_counts_enabled_rows_and_rules() {
        let state = PersistedState {
            upstreams: vec![
                sample_upstream(true, 2, UpstreamProtocol::Responses),
                sample_upstream(false, 0, UpstreamProtocol::ChatCompletions),
            ],
            downstreams: vec![sample_downstream(true)],
            usage_logs: vec![],
        };

        let summary = upstream_summary(&state);
        assert_eq!(summary.total, 2);
        assert_eq!(summary.active, 1);
        assert_eq!(summary.responses, 1);
        assert_eq!(summary.alias_rules, 2);
        assert_eq!(summary.cost_rules, 2);
    }

    #[test]
    fn upstream_rows_show_aliases_and_costs() {
        let state = PersistedState {
            upstreams: vec![sample_upstream(true, 1, UpstreamProtocol::Responses)],
            downstreams: vec![],
            usage_logs: vec![],
        };

        let rows = upstream_rows(&state);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].models, "glm-5");
        assert_eq!(rows[0].alias_summary, "别名：glm-5=GLM-5");
        assert_eq!(rows[0].cost_summary, "计费：glm-5=2");
        assert_eq!(rows[0].status_label, "启用 · 1 次失败");
        assert_eq!(rows[0].status_class, "badge badge-success");
    }

    #[test]
    fn upstream_rows_include_edit_links() {
        let state = PersistedState {
            upstreams: vec![sample_upstream(true, 0, UpstreamProtocol::Responses)],
            downstreams: vec![],
            usage_logs: vec![],
        };

        let rows = upstream_rows(&state);
        assert_eq!(rows[0].edit_href, "/admin/upstreams?edit=up-1");
    }
}
