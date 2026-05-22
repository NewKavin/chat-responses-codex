use leptos::prelude::*;

use gateway_core::routing::UpstreamProtocol;
use gateway_core::state::{AppConfig, PersistedState};

use crate::shell::{AppLayout, Panel, Section, StatCard, Tone};

#[component]
pub fn PortalPage(config: AppConfig, state: PersistedState) -> impl IntoView {
    let app_name = config.app_name.clone();
    let current_session = current_session_card(&state);
    let model_rows = portal_model_rows(&state);
    let active_downstreams = state
        .downstreams
        .iter()
        .filter(|downstream| downstream.active)
        .count();
    let summary = portal_summary(active_downstreams, &model_rows);

    view! {
        <AppLayout
            title="自助门户"
            subtitle=format!("{app_name} 的自助门户，展示下游可见模型和路由能力。")
            active=Section::Portal
        >
            <section class="summary-grid">
                <StatCard
                    label="活跃密钥"
                    value=summary.active_downstreams.to_string()
                    hint="当前可供下游使用的会话"
                    tone=Tone::Teal
                />
                <StatCard
                    label="可见模型"
                    value=summary.visible_models.to_string()
                    hint="下游白名单里的唯一模型"
                    tone=Tone::Blue
                />
                <StatCard
                    label="Responses 支持"
                    value=summary.responses_ready.to_string()
                    hint="可直接走 Responses 的模型"
                    tone=Tone::Gold
                />
                <StatCard
                    label="Chat 支持"
                    value=summary.chat_ready.to_string()
                    hint="仅能通过 ChatCompletions 提供的模型"
                    tone=Tone::Rose
                />
            </section>

            <Panel title="当前会话" subtitle="门户只展示下游视图，不参与网关决策。">
                <div class="section-stack">
                  <div class="note">
                    {current_session.note}
                  </div>
                  <div class="table-shell">
                    <table class="table">
                      <thead>
                        <tr>
                          <th>名称</th>
                          <th>密钥预览</th>
                          <th>模型</th>
                          <th>限制</th>
                          <th>状态</th>
                        </tr>
                      </thead>
                      <tbody>
                        <tr>
                          <td><strong>{current_session.name}</strong></td>
                          <td>{current_session.secret_preview}</td>
                          <td>{current_session.models}</td>
                          <td>{current_session.limits}</td>
                          <td><span class=current_session.status_class>{current_session.status_label}</span></td>
                        </tr>
                      </tbody>
                    </table>
                  </div>
                </div>
            </Panel>

            <Panel title="模型目录" subtitle="展示每个模型的下游覆盖和上游支持能力。">
                <div class="table-shell">
                  <table class="table">
                    <thead>
                      <tr>
                        <th>模型</th>
                        <th>下游覆盖</th>
                        <th>支持协议</th>
                        <th>路由建议</th>
                      </tr>
                    </thead>
                    <tbody>
                      {model_rows
                          .into_iter()
                          .map(|row| view! {
                              <tr>
                                <td><strong>{row.model}</strong></td>
                                <td>{row.downstreams}</td>
                                <td><span class=row.status_class>{row.support_label}</span></td>
                                <td>{row.routing_note}</td>
                              </tr>
                          })
                          .collect::<Vec<_>>()}
                    </tbody>
                  </table>
                </div>
            </Panel>

            <Panel title="接入示例" subtitle="这部分只负责呈现，真实鉴权仍在后端完成。">
                <div class="section-stack">
                  <div class="note">{"门户不会拦截模型请求，只读取下游配置并提示当前能用哪些模型。"}</div>
                  <pre class="code-block">{current_session.curl_example}</pre>
                </div>
            </Panel>
        </AppLayout>
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PortalSummary {
    active_downstreams: usize,
    visible_models: usize,
    responses_ready: usize,
    chat_ready: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PortalCurrentSession {
    name: String,
    secret_preview: String,
    models: String,
    limits: String,
    status_label: String,
    status_class: String,
    note: String,
    curl_example: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PortalModelRow {
    model: String,
    downstreams: String,
    support_label: String,
    status_class: String,
    routing_note: String,
}

fn portal_summary(active_downstreams: usize, model_rows: &[PortalModelRow]) -> PortalSummary {
    let visible_models = model_rows.len();
    let responses_ready = model_rows
        .iter()
        .filter(|row| row.support_label == "Responses")
        .count();
    let chat_ready = model_rows
        .iter()
        .filter(|row| row.support_label == "ChatCompletions")
        .count();

    PortalSummary {
        active_downstreams,
        visible_models,
        responses_ready,
        chat_ready,
    }
}

fn current_session_card(state: &PersistedState) -> PortalCurrentSession {
    let current = state
        .downstreams
        .iter()
        .find(|downstream| downstream.active)
        .or_else(|| state.downstreams.first());

    match current {
        Some(downstream) => {
            let secret_preview = preview_secret(downstream.plaintext_key.as_deref());
            let models = if downstream.model_allowlist.is_empty() {
                "无模型白名单".to_string()
            } else {
                downstream.model_allowlist.join(", ")
            };
            let limits = format!(
                "{} /min · 日 {} · 月 {}",
                downstream.per_minute_limit,
                downstream
                    .daily_token_limit
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "无限".to_string()),
                downstream
                    .monthly_token_limit
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "无限".to_string()),
            );
            let status_label = if downstream.active {
                "启用".to_string()
            } else {
                "停用".to_string()
            };
            let status_class = if downstream.active {
                "badge badge-success".to_string()
            } else {
                "badge badge-warning".to_string()
            };
            let note = if downstream.active {
                "当前门户默认展示第一个启用中的下游会话。".to_string()
            } else {
                "当前没有启用中的下游，展示的是一个备用记录。".to_string()
            };
            let curl_example = format!(
                "curl -H 'Authorization: Bearer {}' \\\n  https://gateway.example.com/v1/responses",
                downstream
                    .plaintext_key
                    .as_deref()
                    .unwrap_or("<downstream-key>")
            );

            PortalCurrentSession {
                name: downstream.name.clone(),
                secret_preview,
                models,
                limits,
                status_label,
                status_class,
                note,
                curl_example,
            }
        }
        None => PortalCurrentSession {
            name: "暂无下游".to_string(),
            secret_preview: "未配置".to_string(),
            models: "无".to_string(),
            limits: "无".to_string(),
            status_label: "空".to_string(),
            status_class: "badge badge-warning".to_string(),
            note: "当前没有可展示的下游密钥，请先去管理员页创建一个。".to_string(),
            curl_example: "curl -H 'Authorization: Bearer <downstream-key>' \\\n  https://gateway.example.com/v1/responses".to_string(),
        },
    }
}

fn portal_model_rows(state: &PersistedState) -> Vec<PortalModelRow> {
    let mut models = std::collections::BTreeMap::<String, Vec<String>>::new();

    for downstream in state
        .downstreams
        .iter()
        .filter(|downstream| downstream.active)
    {
        for model in &downstream.model_allowlist {
            models
                .entry(model.clone())
                .or_default()
                .push(downstream.name.clone());
        }
    }

    models
        .into_iter()
        .map(|(model, downstreams)| {
            let downstreams = unique_join(&downstreams);
            let (support_label, status_class, routing_note) = model_support(state, &model);

            PortalModelRow {
                model,
                downstreams,
                support_label,
                status_class,
                routing_note,
            }
        })
        .collect()
}

fn model_support(state: &PersistedState, model: &str) -> (String, String, String) {
    let responses_supported = state.upstreams.iter().any(|upstream| {
        upstream.active
            && upstream.protocol == UpstreamProtocol::Responses
            && upstream.supports_model(model)
    });
    if responses_supported {
        return (
            "Responses".to_string(),
            "badge badge-success".to_string(),
            "原生 Responses 路径可用".to_string(),
        );
    }

    let chat_supported = state.upstreams.iter().any(|upstream| {
        upstream.active
            && upstream.protocol == UpstreamProtocol::ChatCompletions
            && upstream.supports_model(model)
    });
    if chat_supported {
        return (
            "ChatCompletions".to_string(),
            "badge badge-info".to_string(),
            "需要通过 Chat 协议提供".to_string(),
        );
    }

    (
        "未配置".to_string(),
        "badge badge-warning".to_string(),
        "需要补充上游或别名映射".to_string(),
    )
}

fn unique_join(values: &[String]) -> String {
    let mut seen = std::collections::BTreeSet::new();
    let mut ordered = Vec::new();

    for value in values {
        if seen.insert(value.clone()) {
            ordered.push(value.clone());
        }
    }

    ordered.join(", ")
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

#[cfg(test)]
mod tests {
    use super::*;
    use gateway_core::routing::UpstreamProtocol;
    use gateway_core::state::{
        DownstreamConfig, ModelAliasConfig, ModelRequestCostConfig, UpstreamConfig,
    };

    fn sample_state() -> PersistedState {
        PersistedState {
            upstreams: vec![
                UpstreamConfig {
                    id: "up-1".into(),
                    name: "Responses".into(),
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
                },
                UpstreamConfig {
                    id: "up-2".into(),
                    name: "Chat".into(),
                    base_url: "https://chat.example.com".into(),
                    api_key: "sk-demo".into(),
                    protocol: UpstreamProtocol::ChatCompletions,
                    supported_models: vec!["gpt-4.1-mini".into()],
                    model_aliases: vec![],
                    request_quota_5h: 600,
                    requests_per_minute: 20,
                    max_concurrency: 4,
                    model_request_costs: vec![],
                    active: true,
                    failure_count: 1,
                },
            ],
            downstreams: vec![
                DownstreamConfig {
                    id: "down-1".into(),
                    name: "Team A".into(),
                    hash: "sha256:demo".into(),
                    plaintext_key: Some("sk-team-a-demo".into()),
                    model_allowlist: vec!["glm-5".into(), "gpt-4.1-mini".into()],
                    per_minute_limit: 20,
                    daily_token_limit: Some(1_000),
                    monthly_token_limit: Some(2_000),
                    request_quota_window_hours: None,
                    request_quota_requests: None,
                    ip_allowlist: vec![],
                    expires_at: None,
                    active: true,
                },
                DownstreamConfig {
                    id: "down-2".into(),
                    name: "Team B".into(),
                    hash: "sha256:demo2".into(),
                    plaintext_key: Some("sk-team-b-demo".into()),
                    model_allowlist: vec!["glm-5".into()],
                    per_minute_limit: 10,
                    daily_token_limit: Some(500),
                    monthly_token_limit: Some(1_000),
                    request_quota_window_hours: None,
                    request_quota_requests: None,
                    ip_allowlist: vec![],
                    expires_at: None,
                    active: false,
                },
            ],
            usage_logs: vec![],
        }
    }

    #[test]
    fn summary_counts_models_and_active_keys() {
        let state = sample_state();
        let model_rows = portal_model_rows(&state);
        let active_downstreams = state
            .downstreams
            .iter()
            .filter(|downstream| downstream.active)
            .count();
        let summary = portal_summary(active_downstreams, &model_rows);
        assert_eq!(summary.active_downstreams, 1);
        assert_eq!(summary.visible_models, 2);
        assert_eq!(summary.responses_ready, 1);
        assert_eq!(summary.chat_ready, 1);
    }

    #[test]
    fn model_rows_join_downstreams_and_support() {
        let rows = portal_model_rows(&sample_state());
        assert_eq!(rows.len(), 2);
        let glm_row = rows.iter().find(|row| row.model == "glm-5").unwrap();
        assert_eq!(glm_row.support_label, "Responses");

        let gpt_row = rows.iter().find(|row| row.model == "gpt-4.1-mini").unwrap();
        assert_eq!(gpt_row.support_label, "ChatCompletions");
    }
}
