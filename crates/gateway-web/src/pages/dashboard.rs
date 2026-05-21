use leptos::prelude::*;

use crate::shell::{AppLayout, Panel, Section, StatCard, Tone};

#[component]
pub fn DashboardPage() -> impl IntoView {
    let stats = [
        (
            "6",
            "上游连接",
            "4 个启用，2 个备用",
            Tone::Teal,
        ),
        (
            "18",
            "下游密钥",
            "3 个会话活跃，15 个待用",
            Tone::Blue,
        ),
        (
            "92",
            "今日请求",
            "Responses 与 Chat 统一记账",
            Tone::Gold,
        ),
        (
            "0",
            "未处理错误",
            "当前无阻断级别异常",
            Tone::Rose,
        ),
    ];

    let recent_events = [
        ("REQ-1041", "glm-5", "/v1/responses", "Chat fallback", "12ms"),
        ("REQ-1042", "gpt-4.1-mini", "/v1/chat/completions", "Direct upstream", "18ms"),
        ("REQ-1043", "glm-5.1", "/v1/responses", "Stream retry", "24ms"),
        ("REQ-1044", "moonshot-v1", "/v1/chat/completions", "Rate check", "9ms"),
    ];

    view! {
        <AppLayout
            title="仪表盘"
            subtitle="协议转换与能力保留控制台的全局概览。"
            active=Section::Dashboard
        >
            <section class="summary-grid">
                {stats
                    .into_iter()
                    .map(|(value, label, hint, tone)| view! {
                        <StatCard label=label value=value hint=hint tone=tone />
                    })
                    .collect::<Vec<_>>()}
            </section>

            <Panel title="最近请求" subtitle="展示最近的协议转换、降级和流式行为。">
                <div class="table-shell">
                  <table class="table">
                    <thead>
                      <tr>
                        <th>请求 ID</th>
                        <th>模型</th>
                        <th>路径</th>
                        <th>结果</th>
                        <th>耗时</th>
                      </tr>
                    </thead>
                    <tbody>
                      {recent_events
                          .into_iter()
                          .map(|(request_id, model, path, result, latency)| view! {
                              <tr>
                                <td><strong>{request_id}</strong></td>
                                <td>{model}</td>
                                <td>{path}</td>
                                <td><span class="badge badge-info">{result}</span></td>
                                <td>{latency}</td>
                              </tr>
                          })
                          .collect::<Vec<_>>()}
                    </tbody>
                  </table>
                </div>
            </Panel>

            <Panel title="运行状态" subtitle="当前 scaffold 保留了核心能力边界。">
                <div class="section-stack">
                  <div class="note">
                    未来的 Leptos UI 会只接管管理后台表现层，协议转换、上游选择、限流和 fallback 仍然留在后端核心。
                  </div>
                  <div class="empty-state">
                    这里后续可放置健康检查、告警摘要和最近的降级事件趋势图。
                  </div>
                </div>
            </Panel>
        </AppLayout>
    }
}

