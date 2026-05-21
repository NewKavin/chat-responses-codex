use leptos::prelude::*;

use crate::shell::{AppLayout, Panel, Section, StatCard, Tone};

#[component]
pub fn PortalPage() -> impl IntoView {
    let stats = [
        ("4", "可见模型", "门户只展示下游允许访问的模型", Tone::Teal),
        ("1", "当前会话", "自助门户会显示当前密钥", Tone::Blue),
        ("20", "每分钟限制", "示例下游默认限制", Tone::Gold),
        ("0", "提示错误", "当前没有校验失败", Tone::Rose),
    ];

    let rows = [
        ("glm-5", "支持 Responses fallback", "推荐"),
        ("glm-5.1", "支持 Responses fallback", "推荐"),
        ("gpt-4.1-mini", "支持 ChatCompletions", "兼容"),
        ("moonshot-v1", "支持 ChatCompletions", "兼容"),
    ];

    view! {
        <AppLayout
            title="自助门户"
            subtitle="下游客户端的可视化入口，后续会连接真实的密钥状态。"
            active=Section::Portal
        >
            <section class="summary-grid">
                {stats
                    .into_iter()
                    .map(|(value, label, hint, tone)| view! {
                        <StatCard label=label value=value hint=hint tone=tone />
                    })
                    .collect::<Vec<_>>()}
            </section>

            <Panel title="门户说明" subtitle="将来这里会展示 key、权限、模型和使用量。">
                <div class="section-stack">
                  <div class="note">
                    门户不承载网关决策，只读取后端给出的下游视图数据。
                  </div>
                  <div class="table-shell">
                    <table class="table">
                      <thead>
                        <tr>
                          <th>模型</th>
                          <th>兼容性</th>
                          <th>说明</th>
                        </tr>
                      </thead>
                      <tbody>
                        {rows
                            .into_iter()
                            .map(|(model, compatibility, note)| view! {
                                <tr>
                                  <td><strong>{model}</strong></td>
                                  <td><span class="badge badge-strong">{compatibility}</span></td>
                                  <td>{note}</td>
                                </tr>
                            })
                            .collect::<Vec<_>>()}
                      </tbody>
                    </table>
                  </div>
                  <pre class="code-block">{"curl -H 'Authorization: Bearer <downstream-key>' \\\n  https://gateway.example.com/v1/models"}</pre>
                </div>
            </Panel>
        </AppLayout>
    }
}

