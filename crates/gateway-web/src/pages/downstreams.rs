use leptos::prelude::*;

use gateway_core::admin::{
    DownstreamFormView, DownstreamLifetimeFilter, DownstreamListQuery, DownstreamStatusFilter,
};
use gateway_core::state::{unix_seconds, AppConfig, DownstreamConfig, PersistedState};

use crate::shell::{AppLayout, DisclosurePanel, Panel, Section, StatCard, Tone};

#[component]
pub fn DownstreamsPage(
    config: AppConfig,
    state: PersistedState,
    form: DownstreamFormView,
    query: DownstreamListQuery,
    notice: String,
    form_open: bool,
) -> impl IntoView {
    let app_name = config.app_name.clone();
    let summary = downstream_summary(&state);
    let rows = downstream_rows(&state, &query);

    let heading = form.heading.clone();
    let action = form.action.clone();
    let submit_label = form.submit_label.clone();
    let name = form.name.clone();
    let models = form.models.clone();
    let per_minute_limit = form.per_minute_limit.clone();
    let daily_token_limit = form.daily_token_limit.clone();
    let monthly_token_limit = form.monthly_token_limit.clone();
    let ip_allowlist = form.ip_allowlist.clone();
    let expires_at = form.expires_at.clone();
    let never_expires = form.never_expires;
    let active = form.active;
    let plaintext_key = form.plaintext_key.clone().unwrap_or_default();
    let secret_note = if form.legacy_secret {
        "旧版本兼容场景：创建后只显示一次明文密钥。"
    } else {
        "新建成功后会返回一次性明文密钥，记得先复制再关闭页面。"
    };

    let search_value = query.search_value();
    let status_filter = query.status_filter();
    let lifetime_filter = query.lifetime_filter();

    view! {
        <AppLayout
            title="下游密钥"
            subtitle=format!("{app_name} 的下游账号、白名单和过期状态。")
            active=Section::Downstreams
        >
            <section class="summary-grid">
                <StatCard
                    label="总下游"
                    value=summary.total.to_string()
                    hint="共享 state 中的下游账号数量"
                    tone=Tone::Teal
                />
                <StatCard
                    label="启用中"
                    value=summary.active.to_string()
                    hint="当前可以直接用于请求的密钥"
                    tone=Tone::Blue
                />
                <StatCard
                    label="可见模型"
                    value=summary.model_bindings.to_string()
                    hint="下游白名单里的模型总数"
                    tone=Tone::Gold
                />
                <StatCard
                    label="过期计划"
                    value=summary.expiring.to_string()
                    hint="需要继续跟进的到期配置"
                    tone=Tone::Rose
                />
            </section>

            <div class="note">
                <strong>提示</strong>
                <span>
                  {format!(
                      "其中 {} 个永不过期，下游白名单里共有 {} 条 IP 规则。",
                      summary.unlimited, summary.ip_rules
                  )}
                </span>
            </div>

            <Panel title="筛选下游" subtitle="筛选器直接绑定到 gateway-core::admin::DownstreamListQuery。">
                <form class="form-grid panel-toolbar" method="get" action="/admin/downstreams">
                  <div class="field">
                    <label for="search">搜索</label>
                    <input id="search" name="search" value=search_value placeholder="名称或明文密钥" />
                  </div>
                  <div class="field">
                    <label for="status">状态</label>
                    <select id="status" name="status">
                      <option selected=matches!(status_filter, DownstreamStatusFilter::All) value="">
                        全部
                      </option>
                      <option selected=matches!(status_filter, DownstreamStatusFilter::Active) value="active">
                        启用
                      </option>
                      <option selected=matches!(status_filter, DownstreamStatusFilter::Inactive) value="inactive">
                        停用
                      </option>
                    </select>
                  </div>
                  <div class="field">
                    <label for="lifetime">生命周期</label>
                    <select id="lifetime" name="lifetime">
                      <option selected=matches!(lifetime_filter, DownstreamLifetimeFilter::All) value="">
                        全部
                      </option>
                      <option
                        selected=matches!(lifetime_filter, DownstreamLifetimeFilter::Unlimited)
                        value="unlimited"
                      >
                        永不过期
                      </option>
                      <option
                        selected=matches!(lifetime_filter, DownstreamLifetimeFilter::Expiring)
                        value="expiring"
                      >
                        即将到期
                      </option>
                    </select>
                  </div>
                  <div class="actions">
                    <button class="button primary" type="submit">应用筛选</button>
                    <a class="button secondary" href="/admin/downstreams">重置筛选</a>
                  </div>
                </form>
            </Panel>

            <DisclosurePanel
                title="下游表单"
                subtitle="点击右上角按钮展开新增或编辑下游。"
                action_label=heading
                open=form_open
            >
                <div class="section-stack">
                  <div class="note">
                    <strong>提示</strong>
                    <span>{notice}</span>
                  </div>
                  <form class="section-stack" method="post" action=action>
                    <div class="form-grid">
                      <div class="field">
                        <label for="downstream-name">名称</label>
                        <input id="downstream-name" name="name" value=name />
                      </div>
                      <div class="field">
                        <label for="per-minute-limit">每分钟限制</label>
                        <input id="per-minute-limit" name="per_minute_limit" type="number" value=per_minute_limit />
                      </div>
                      <div class="field">
                        <label for="daily-token-limit">每日 Token 限制</label>
                        <input id="daily-token-limit" name="daily_token_limit" type="number" value=daily_token_limit />
                      </div>
                      <div class="field">
                        <label for="monthly-token-limit">每月 Token 限制</label>
                        <input id="monthly-token-limit" name="monthly_token_limit" type="number" value=monthly_token_limit />
                        <span class="hint">{"如果该下游使用请求次数限额，这两项只作为参考数据保留，不参与实际拦截。"}</span>
                      </div>
                      <div class="field">
                        <div>
                          <label for="models">模型白名单</label>
                          <span class="hint">{"每行一个模型，后续会按 core 侧路由规则做统一归一。"}</span>
                        </div>
                        <textarea id="models" name="models" placeholder="glm-5&#10;glm-5.1">{models}</textarea>
                      </div>
                      <div class="field">
                        <div>
                          <label for="ip-allowlist">IP 白名单</label>
                          <span class="hint">{"留空表示不限制。"}</span>
                        </div>
                        <textarea id="ip-allowlist" name="ip_allowlist" placeholder="10.0.0.0/24">{ip_allowlist}</textarea>
                      </div>
                      <div class="field">
                        <label for="expires-at">过期时间</label>
                        <input id="expires-at" name="expires_at" placeholder="Unix 秒或留空" value=expires_at />
                      </div>
                      <div class="field">
                        <div>
                          <label for="plaintext-key">明文密钥</label>
                          <span class="hint">{secret_note}</span>
                        </div>
                        <input id="plaintext-key" value=plaintext_key readonly />
                      </div>
                      <div class="field">
                        <div>
                          <label for="never-expires">永不过期</label>
                          <input id="never-expires" name="never_expires" type="checkbox" checked=never_expires />
                          <span class="hint">{"勾选后会忽略过期时间。"}</span>
                        </div>
                      </div>
                      <div class="field">
                        <div>
                          <label for="downstream-active">启用状态</label>
                          <input id="downstream-active" name="active" type="checkbox" checked=active />
                          <span class="hint">{"关闭后配置仍保留，只是不再参与路由。"}</span>
                        </div>
                      </div>
                    </div>
                    <div class="actions">
                      <button class="button primary" type="submit">{submit_label}</button>
                    </div>
                  </form>
                </div>
            </DisclosurePanel>

            <Panel
                title="下游列表"
                subtitle=format!("当前匹配 {} 条记录，数据来自 core state。", rows.len())
            >
                <div class="table-shell">
                  <table class="table">
                    <thead>
                      <tr>
                        <th>名称</th>
                        <th>密钥</th>
                        <th>模型</th>
                        <th>限制</th>
                        <th>生命周期</th>
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
                                  <div class="hint">{row.id}</div>
                                </td>
                                <td>
                                  <strong>{row.secret_preview}</strong>
                                  <div class="hint">{row.secret_hint}</div>
                                </td>
                                <td>
                                  <strong>{row.models}</strong>
                                  <div class="hint">{row.ip_summary}</div>
                                </td>
                                <td>
                                  <strong>{row.limit_summary}</strong>
                                  <div class="hint">{row.quota_summary}</div>
                                </td>
                                <td>{row.expiry_label}</td>
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
struct DownstreamSummary {
    total: usize,
    active: usize,
    expiring: usize,
    unlimited: usize,
    model_bindings: usize,
    ip_rules: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DownstreamRow {
    id: String,
    name: String,
    secret_preview: String,
    secret_hint: String,
    models: String,
    ip_summary: String,
    limit_summary: String,
    quota_summary: String,
    expiry_label: String,
    status_label: String,
    status_class: String,
    edit_href: String,
}

fn downstream_summary(state: &PersistedState) -> DownstreamSummary {
    let total = state.downstreams.len();
    let active = state
        .downstreams
        .iter()
        .filter(|downstream| downstream.active)
        .count();
    let expiring = state
        .downstreams
        .iter()
        .filter(|downstream| downstream.expires_at.is_some())
        .count();
    let unlimited = total.saturating_sub(expiring);
    let model_bindings = state
        .downstreams
        .iter()
        .map(|downstream| downstream.model_allowlist.len())
        .sum();
    let ip_rules = state
        .downstreams
        .iter()
        .map(|downstream| downstream.ip_allowlist.len())
        .sum();

    DownstreamSummary {
        total,
        active,
        expiring,
        unlimited,
        model_bindings,
        ip_rules,
    }
}

fn downstream_rows(state: &PersistedState, query: &DownstreamListQuery) -> Vec<DownstreamRow> {
    let now = unix_seconds();

    state
        .downstreams
        .iter()
        .filter(|downstream| query.matches(downstream))
        .map(|downstream| DownstreamRow {
            id: downstream.id.clone(),
            name: downstream.name.clone(),
            secret_preview: preview_secret(downstream.plaintext_key.as_deref()),
            secret_hint: if downstream.plaintext_key.is_some() {
                "明文已保存，可在编辑里查看".to_string()
            } else {
                "创建后会显示一次性明文密钥".to_string()
            },
            models: downstream.model_allowlist.join(", "),
            ip_summary: if downstream.ip_allowlist.is_empty() {
                "IP 白名单：无".to_string()
            } else {
                format!("IP 白名单：{}", downstream.ip_allowlist.join(", "))
            },
            limit_summary: downstream_limit_summary(downstream),
            quota_summary: downstream_quota_summary(downstream),
            expiry_label: downstream_expiry_label(downstream, now),
            status_label: if downstream.active {
                "启用".to_string()
            } else {
                "停用".to_string()
            },
            status_class: if downstream.active {
                "badge badge-success".to_string()
            } else {
                "badge badge-warning".to_string()
            },
            edit_href: downstream_edit_href(query, &downstream.id),
        })
        .collect()
}

fn downstream_limit_summary(downstream: &DownstreamConfig) -> String {
    if downstream.uses_request_quota() {
        format!("{} /min · 请求次数限额", downstream.per_minute_limit)
    } else {
        format!("{} /min · Token 限额", downstream.per_minute_limit)
    }
}

fn downstream_quota_summary(downstream: &DownstreamConfig) -> String {
    let daily_token_limit = downstream
        .daily_token_limit
        .map(|value| value.to_string())
        .unwrap_or_else(|| "无限".to_string());
    let monthly_token_limit = downstream
        .monthly_token_limit
        .map(|value| value.to_string())
        .unwrap_or_else(|| "无限".to_string());

    if downstream.uses_request_quota() {
        let window_hours = downstream.request_quota_window_hours.unwrap_or(0);
        let request_quota_requests = downstream.request_quota_requests.unwrap_or(0);
        format!(
            "请求窗口 {} 小时 / {} 次 · 每日 Token 参考 {} · 每月 Token 参考 {}",
            window_hours, request_quota_requests, daily_token_limit, monthly_token_limit
        )
    } else {
        format!(
            "每日 Token 限额 {} · 每月 Token 限额 {}",
            daily_token_limit, monthly_token_limit
        )
    }
}

fn downstream_edit_href(query: &DownstreamListQuery, downstream_id: &str) -> String {
    let suffix = query.query_suffix();
    if suffix.is_empty() {
        format!("/admin/downstreams?edit={downstream_id}")
    } else {
        format!("/admin/downstreams{suffix}&edit={downstream_id}")
    }
}

fn preview_secret(secret: Option<&str>) -> String {
    let Some(secret) = secret else {
        return "未保存".to_string();
    };

    if secret.len() <= 8 {
        return secret.to_string();
    }

    let head = &secret[..4];
    let tail = &secret[secret.len().saturating_sub(4)..];
    format!("{head}…{tail}")
}

fn downstream_expiry_label(downstream: &DownstreamConfig, now: u64) -> String {
    match downstream.expires_at {
        Some(expires_at) => {
            let remaining = expires_at.saturating_sub(now);
            if remaining == 0 {
                "已到期".to_string()
            } else {
                let days = remaining / 86_400;
                if days == 0 {
                    "24 小时内到期".to_string()
                } else {
                    format!("{} 天后到期", days)
                }
            }
        }
        None => "永不过期".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            downstreams: vec![
                DownstreamConfig {
                    id: "down-1".into(),
                    name: "Team A".into(),
                    hash: "sha256:demo".into(),
                    plaintext_key: Some("sk-team-a-demo".into()),
                    model_allowlist: vec!["glm-5".into(), "glm-5.1".into()],
                    per_minute_limit: 20,
                    daily_token_limit: Some(100_000),
                    monthly_token_limit: Some(200_000),
                    request_quota_window_hours: None,
                    request_quota_requests: None,
                    ip_allowlist: vec!["127.0.0.1".into()],
                    expires_at: None,
                    active: true,
                },
                DownstreamConfig {
                    id: "down-2".into(),
                    name: "Team B".into(),
                    hash: "sha256:demo2".into(),
                    plaintext_key: Some("sk-team-b-demo".into()),
                    model_allowlist: vec!["gpt-4.1-mini".into()],
                    per_minute_limit: 10,
                    daily_token_limit: Some(50_000),
                    monthly_token_limit: Some(100_000),
                    request_quota_window_hours: None,
                    request_quota_requests: None,
                    ip_allowlist: vec![],
                    expires_at: Some(1_725_000_000),
                    active: false,
                },
            ],
            usage_logs: vec![UsageLog {
                id: "log-1".into(),
                downstream_key_id: "down-1".into(),
                upstream_key_id: "up-1".into(),
                endpoint: "/v1/responses".into(),
                model: "glm-5".into(),
                request_id: "REQ-1".into(),
                status_code: 200,
                prompt_tokens: 1,
                completion_tokens: 1,
                total_tokens: 2,
                latency_ms: 12,
                created_at: 1_725_000_100,
            }],
        }
    }

    #[test]
    fn summary_counts_downstreams() {
        let summary = downstream_summary(&sample_state());
        assert_eq!(summary.total, 2);
        assert_eq!(summary.active, 1);
        assert_eq!(summary.expiring, 1);
        assert_eq!(summary.unlimited, 1);
        assert_eq!(summary.model_bindings, 3);
        assert_eq!(summary.ip_rules, 1);
    }

    #[test]
    fn rows_filter_by_query() {
        let state = sample_state();
        let query = DownstreamListQuery {
            search: Some("team a".into()),
            status: Some("active".into()),
            lifetime: Some("unlimited".into()),
        };

        let rows = downstream_rows(&state, &query);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Team A");
        assert_eq!(rows[0].secret_hint, "明文已保存，可在编辑里查看");
        assert_eq!(rows[0].status_label, "启用");
        assert_eq!(rows[0].expiry_label, "永不过期");
    }

    #[test]
    fn rows_include_edit_links_that_preserve_filters() {
        let state = sample_state();
        let query = DownstreamListQuery {
            search: Some("team".into()),
            status: Some("active".into()),
            lifetime: Some("unlimited".into()),
        };

        let rows = downstream_rows(&state, &query);
        assert_eq!(
            rows[0].edit_href,
            "/admin/downstreams?search=team&status=active&lifetime=unlimited&edit=down-1"
        );
    }

    #[test]
    fn rows_show_request_quota_mode_with_token_reference_values() {
        let state = PersistedState {
            downstreams: vec![DownstreamConfig {
                id: "down-request".into(),
                name: "Request Team".into(),
                hash: "sha256:request".into(),
                plaintext_key: Some("sk-request-demo".into()),
                model_allowlist: vec!["glm-5".into()],
                per_minute_limit: 30,
                daily_token_limit: Some(12_345),
                monthly_token_limit: Some(67_890),
                request_quota_window_hours: Some(6),
                request_quota_requests: Some(400),
                ip_allowlist: vec![],
                expires_at: None,
                active: true,
            }],
            ..PersistedState::default()
        };

        let rows = downstream_rows(&state, &DownstreamListQuery::default());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].limit_summary, "30 /min · 请求次数限额");
        assert_eq!(
            rows[0].quota_summary,
            "请求窗口 6 小时 / 400 次 · 每日 Token 参考 12345 · 每月 Token 参考 67890"
        );
    }

    #[test]
    fn rendered_page_uses_compact_toolbar_and_inline_notices() {
        let html = crate::app::render_downstreams_page(DownstreamListQuery::default(), None).0;

        assert!(html.contains(r#"class="form-grid panel-toolbar""#));
        assert!(html.contains("<strong>提示</strong>"));
        assert!(html.contains("参考数据"));
    }
}
