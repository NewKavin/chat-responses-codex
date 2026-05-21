use leptos::prelude::*;

use crate::shell::{AppLayout, Panel, Section, StatCard, Tone};

#[component]
pub fn UpstreamsPage() -> impl IntoView {
    let stats = [
        ("4", "启用上游", "可参与模型路由", Tone::Teal),
        ("9", "模型映射", "别名与上游模型对应", Tone::Blue),
        ("3", "Responses 上游", "支持 Responses 原生协议", Tone::Gold),
        ("2", "告警项", "待观察的失败计数", Tone::Rose),
    ];

    let rows = [
        (
            "primary-glm",
            "https://api.example.com",
            "ChatCompletions",
            "glm-5, glm-5.1",
            "600 / 20 / 4",
            "启用",
        ),
        (
            "primary-openai",
            "https://api.openai.example.com",
            "Responses",
            "gpt-4.1-mini, gpt-4.1",
            "600 / 20 / 4",
            "启用",
        ),
        (
            "backup-legacy",
            "https://legacy.example.com",
            "ChatCompletions",
            "gpt-3.5-turbo",
            "600 / 20 / 4",
            "备用",
        ),
    ];

    view! {
        <AppLayout
            title="上游密钥"
            subtitle="配置上游 URL、协议类型、模型映射与请求成本。"
            active=Section::Upstreams
        >
            <section class="summary-grid">
                {stats
                    .into_iter()
                    .map(|(value, label, hint, tone)| view! {
                        <StatCard label=label value=value hint=hint tone=tone />
                    })
                    .collect::<Vec<_>>()}
            </section>

            <Panel title="新增或编辑上游" subtitle="这里保留了后续要接入的完整字段形状。">
                <form class="section-stack" method="post" action="/admin/upstreams">
                  <div class="form-grid">
                    <div class="field">
                      <label for="upstream-name">名称</label>
                      <input id="upstream-name" name="name" value="primary-glm">
                    </div>
                    <div class="field">
                      <label for="upstream-base-url">Base URL</label>
                      <input id="upstream-base-url" name="base_url" value="https://api.example.com">
                    </div>
                    <div class="field">
                      <label for="upstream-api-key">API Key</label>
                      <input id="upstream-api-key" name="api_key" type="password" value="sk-placeholder">
                    </div>
                    <div class="field">
                      <label for="upstream-protocol">协议</label>
                      <select id="upstream-protocol" name="protocol">
                        <option>ChatCompletions</option>
                        <option>Responses</option>
                      </select>
                    </div>
                    <div class="field">
                      <label for="request-quota">5小时请求上限</label>
                      <input id="request-quota" name="request_quota_5h" type="number" value="600">
                    </div>
                    <div class="field">
                      <label for="requests-per-minute">每分钟请求上限</label>
                      <input id="requests-per-minute" name="requests_per_minute" type="number" value="20">
                    </div>
                    <div class="field">
                      <label for="max-concurrency">最大并发</label>
                      <input id="max-concurrency" name="max_concurrency" type="number" value="4">
                    </div>
                    <div class="field">
                      <label for="model-request-costs">模型请求成本</label>
                      <textarea id="model-request-costs" name="model_request_costs" placeholder="glm-5=2&#10;glm-5.1=2">glm-5=2&#10;glm-5.1=2</textarea>
                    </div>
                  </div>
                  <div class="actions">
                    <button class="button primary" type="submit">保存上游</button>
                    <a class="button secondary" href="/admin/upstreams/new">新增上游</a>
                  </div>
                </form>
            </Panel>

            <Panel title="上游列表" subtitle="展示协议、模型和计费规则，方便后续迁移到真实数据。">
                <div class="table-shell">
                  <table class="table">
                    <thead>
                      <tr>
                        <th>名称</th>
                        <th>Base URL</th>
                        <th>协议</th>
                        <th>模型</th>
                        <th>限额</th>
                        <th>状态</th>
                      </tr>
                    </thead>
                    <tbody>
                      {rows
                          .into_iter()
                          .map(|(name, base_url, protocol, models, quota, status)| view! {
                              <tr>
                                <td><strong>{name}</strong></td>
                                <td>{base_url}</td>
                                <td><span class="badge badge-muted">{protocol}</span></td>
                                <td>{models}</td>
                                <td>{quota}</td>
                                <td><span class="badge badge-success">{status}</span></td>
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

