use leptos::prelude::*;

use crate::shell::{AppLayout, Panel, Section, StatCard, Tone};

#[component]
pub fn LogsPage() -> impl IntoView {
    let stats = [
        ("12", "运行日志", "最近一小时内的关键事件", Tone::Teal),
        ("2", "降级日志", "Responses fallback 可见", Tone::Blue),
        ("1", "鉴权错误", "需要继续观察的异常请求", Tone::Gold),
        ("0", "阻断错误", "当前没有阻塞级失败", Tone::Rose),
    ];

    let rows = [
        (
            "2026-05-21 09:14:02",
            "INFO",
            "dispatch",
            "REQ-1041",
            "responses request sanitized before ChatCompletions conversion",
        ),
        (
            "2026-05-21 09:15:08",
            "WARN",
            "dispatch",
            "REQ-1043",
            "stream retry attempted after upstream read timeout",
        ),
        (
            "2026-05-21 09:16:40",
            "INFO",
            "auth",
            "down-1",
            "downstream key verified and session accepted",
        ),
        (
            "2026-05-21 09:17:22",
            "WARN",
            "routing",
            "glm-5",
            "fallback to ChatCompletions because no Responses upstream supports the model",
        ),
    ];

    view! {
        <AppLayout
            title="运行日志"
            subtitle="查看请求、降级、鉴权和流式重试记录。"
            active=Section::Logs
        >
            <section class="summary-grid">
                {stats
                    .into_iter()
                    .map(|(value, label, hint, tone)| view! {
                        <StatCard label=label value=value hint=hint tone=tone />
                    })
                    .collect::<Vec<_>>()}
            </section>

            <Panel title="日志列表" subtitle="这张表后续将直接接入结构化运行日志。">
                <div class="table-shell">
                  <table class="table">
                    <thead>
                      <tr>
                        <th>时间</th>
                        <th>级别</th>
                        <th>来源</th>
                        <th>请求 ID</th>
                        <th>消息</th>
                      </tr>
                    </thead>
                    <tbody>
                      {rows
                          .into_iter()
                          .map(|(time, level, source, request_id, message)| view! {
                              <tr>
                                <td>{time}</td>
                                <td>
                                  <span class=move || if level == "WARN" {
                                      "badge badge-warning"
                                  } else {
                                      "badge badge-muted"
                                  }>
                                    {level}
                                  </span>
                                </td>
                                <td>{source}</td>
                                <td>{request_id}</td>
                                <td>{message}</td>
                              </tr>
                          })
                          .collect::<Vec<_>>()}
                    </tbody>
                  </table>
                </div>
            </Panel>

            <Panel title="排障提示" subtitle="把结构化日志和 UI 视图先对齐。">
                <div class="section-stack">
                  <div class="note">
                    这里后续可以加入 request_id 过滤、级别筛选和导出按钮。
                  </div>
                  <div class="code-block">{"[info] request started\n[warn] responses request sanitized before ChatCompletions conversion\n[info] request completed"}</div>
                </div>
            </Panel>
        </AppLayout>
    }
}

