use leptos::prelude::*;

use crate::shell::{AppLayout, Panel, Section, StatCard, Tone};

#[component]
pub fn DownstreamsPage() -> impl IntoView {
    let stats = [
        ("18", "下游密钥", "3 个活跃会话", Tone::Teal),
        ("12", "支持模型", "按下游白名单过滤", Tone::Blue),
        ("4", "分钟限制", "当前页面中最关键的阈值", Tone::Gold),
        ("2", "待轮换", "需要替换的旧密钥", Tone::Rose),
    ];

    let rows = [
        (
            "team-a",
            "sk-***a1b2",
            "glm-5, glm-5.1",
            "20 / 100000 / 200000",
            "永不过期",
            "启用",
        ),
        (
            "team-b",
            "sk-***c3d4",
            "gpt-4.1-mini",
            "10 / 50000 / 100000",
            "2026-06-30",
            "启用",
        ),
        (
            "legacy-lab",
            "sk-***e5f6",
            "glm-4.6",
            "5 / 10000 / 20000",
            "2026-05-31",
            "待轮换",
        ),
    ];

    view! {
        <AppLayout
            title="下游密钥"
            subtitle="管理客户密钥、模型白名单、Token 限制和过期状态。"
            active=Section::Downstreams
        >
            <section class="summary-grid">
                {stats
                    .into_iter()
                    .map(|(value, label, hint, tone)| view! {
                        <StatCard label=label value=value hint=hint tone=tone />
                    })
                    .collect::<Vec<_>>()}
            </section>

            <Panel title="新增或编辑下游" subtitle="保留秘密字段、模型白名单和限制字段的完整形状。">
                <form class="section-stack" method="post" action="/admin/downstreams">
                  <div class="form-grid">
                    <div class="field">
                      <label for="downstream-name">名称</label>
                      <input id="downstream-name" name="name" value="team-a">
                    </div>
                    <div class="field">
                      <label for="per-minute-limit">每分钟限制</label>
                      <input id="per-minute-limit" name="per_minute_limit" type="number" value="20">
                    </div>
                    <div class="field">
                      <label for="daily-token-limit">每日 Token 限制</label>
                      <input id="daily-token-limit" name="daily_token_limit" type="number" value="100000">
                    </div>
                    <div class="field">
                      <label for="monthly-token-limit">每月 Token 限制</label>
                      <input id="monthly-token-limit" name="monthly_token_limit" type="number" value="200000">
                    </div>
                    <div class="field">
                      <label for="model-allowlist">模型白名单</label>
                      <textarea id="model-allowlist" name="model_allowlist" placeholder="glm-5&#10;glm-5.1">glm-5&#10;glm-5.1</textarea>
                      <span class="hint">每行一个模型，后续可自动映射大写或别名。</span>
                    </div>
                    <div class="field">
                      <label for="ip-allowlist">IP 白名单</label>
                      <textarea id="ip-allowlist" name="ip_allowlist" placeholder="10.0.0.0/24"></textarea>
                      <span class="hint">留空表示不限制。</span>
                    </div>
                  </div>
                  <div class="form-grid">
                    <div class="field">
                      <label for="expires-at">过期时间</label>
                      <input id="expires-at" name="expires_at" placeholder="永不过期或 2026-06-30 12:00">
                    </div>
                    <div class="field">
                      <label for="secret-note">密钥状态</label>
                      <input id="secret-note" value="plaintext key will be shown after creation" readonly>
                    </div>
                  </div>
                  <div class="actions">
                    <button class="button primary" type="submit">保存下游</button>
                    <a class="button secondary" href="/admin/downstreams/new">新增下游</a>
                  </div>
                </form>
            </Panel>

            <Panel title="下游列表" subtitle="保留查看、复制、重置和删除等后续操作入口。">
                <div class="table-shell">
                  <table class="table">
                    <thead>
                      <tr>
                        <th>名称</th>
                        <th>密钥预览</th>
                        <th>模型</th>
                        <th>限制</th>
                        <th>过期</th>
                        <th>状态</th>
                      </tr>
                    </thead>
                    <tbody>
                      {rows
                          .into_iter()
                          .map(|(name, secret, models, limits, expires, status)| view! {
                              <tr>
                                <td><strong>{name}</strong></td>
                                <td>{secret}</td>
                                <td>{models}</td>
                                <td>{limits}</td>
                                <td>{expires}</td>
                                <td><span class="badge badge-info">{status}</span></td>
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

