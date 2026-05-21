use leptos::prelude::*;

pub const APP_NAME: &str = "chat-responses-codex";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Dashboard,
    Upstreams,
    Downstreams,
    Logs,
    Portal,
}

#[derive(Clone, Copy)]
pub struct NavItem {
    pub section: Section,
    pub href: &'static str,
    pub title: &'static str,
    pub subtitle: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Teal,
    Blue,
    Gold,
    Rose,
}

impl Tone {
    fn class(self) -> &'static str {
        match self {
            Self::Teal => "teal",
            Self::Blue => "blue",
            Self::Gold => "gold",
            Self::Rose => "rose",
        }
    }
}

impl Section {
    pub const fn nav_items() -> [NavItem; 5] {
        [
            NavItem {
                section: Section::Dashboard,
                href: "/admin",
                title: "仪表盘",
                subtitle: "全局概览",
            },
            NavItem {
                section: Section::Upstreams,
                href: "/admin/upstreams",
                title: "上游密钥",
                subtitle: "模型路由",
            },
            NavItem {
                section: Section::Downstreams,
                href: "/admin/downstreams",
                title: "下游密钥",
                subtitle: "客户密钥",
            },
            NavItem {
                section: Section::Logs,
                href: "/admin/logs",
                title: "运行日志",
                subtitle: "审计与排障",
            },
            NavItem {
                section: Section::Portal,
                href: "/portal",
                title: "自助门户",
                subtitle: "下游视图",
            },
        ]
    }
}

pub const APP_CSS: &str = r#"
:root {
  color-scheme: light;
  --bg: #eef4f8;
  --bg-2: #f8fbfd;
  --panel: rgba(255, 255, 255, 0.88);
  --panel-strong: #ffffff;
  --border: rgba(15, 23, 42, 0.08);
  --border-strong: rgba(15, 23, 42, 0.14);
  --text: #102033;
  --muted: #617085;
  --accent: #0fa3b1;
  --accent-2: #2f7cf6;
  --accent-3: #7c5cff;
  --accent-4: #ef7d57;
  --shadow: 0 30px 70px rgba(15, 23, 42, 0.08);
}

* { box-sizing: border-box; }
html, body { min-height: 100%; }
body {
  margin: 0;
  color: var(--text);
  font-family: "Manrope", "Avenir Next", "Segoe UI", sans-serif;
  background:
    radial-gradient(circle at top left, rgba(47, 124, 246, 0.18), transparent 30%),
    radial-gradient(circle at top right, rgba(15, 163, 177, 0.14), transparent 32%),
    linear-gradient(180deg, #f8fbfe 0%, #eef4f8 100%);
}

a { color: inherit; text-decoration: none; }
button, input, select, textarea { font: inherit; }
button { cursor: pointer; }

.page-root {
  min-height: 100vh;
  display: grid;
  grid-template-columns: 286px minmax(0, 1fr);
}

.sidebar {
  padding: 28px 22px;
  color: #f6fbff;
  background:
    linear-gradient(180deg, rgba(10, 18, 34, 0.97), rgba(11, 23, 38, 0.95)),
    radial-gradient(circle at top right, rgba(15, 163, 177, 0.18), transparent 40%);
  display: flex;
  flex-direction: column;
  gap: 24px;
  border-right: 1px solid rgba(255, 255, 255, 0.06);
}

.brand {
  display: grid;
  gap: 8px;
}

.brand-kicker {
  text-transform: uppercase;
  letter-spacing: 0.2em;
  font-size: 11px;
  color: rgba(203, 213, 225, 0.72);
}

.brand strong {
  font-size: 22px;
  line-height: 1.15;
}

.brand p,
.muted {
  color: var(--muted);
}

.sidebar .muted,
.sidebar .brand p {
  color: rgba(226, 232, 240, 0.72);
}

.nav {
  display: grid;
  gap: 10px;
}

.nav-item {
  display: block;
  padding: 14px 16px;
  border-radius: 18px;
  border: 1px solid rgba(255, 255, 255, 0.08);
  background: rgba(255, 255, 255, 0.03);
  transition: transform 120ms ease, border-color 120ms ease, background 120ms ease;
}

.nav-item:hover {
  transform: translateY(-1px);
  border-color: rgba(255, 255, 255, 0.18);
  background: rgba(255, 255, 255, 0.06);
}

.nav-item.active {
  border-color: rgba(76, 201, 240, 0.42);
  background: linear-gradient(135deg, rgba(15, 163, 177, 0.24), rgba(47, 124, 246, 0.20));
  box-shadow: 0 16px 34px rgba(15, 163, 177, 0.18);
}

.nav-item strong,
.nav-item small {
  display: block;
}

.nav-item strong {
  font-size: 15px;
}

.nav-item small {
  margin-top: 4px;
  color: rgba(226, 232, 240, 0.72);
}

.sidebar-footer {
  margin-top: auto;
}

.sidebar-footer-card {
  padding: 18px;
  border-radius: 20px;
  border: 1px solid rgba(255, 255, 255, 0.08);
  background: rgba(255, 255, 255, 0.04);
}

.sidebar-footer-card strong {
  display: block;
  margin-bottom: 8px;
}

.main {
  padding: 28px;
}

.page-header {
  display: flex;
  align-items: flex-end;
  justify-content: space-between;
  gap: 16px;
  margin-bottom: 18px;
}

.page-header h1,
.page-header h2 {
  margin: 0;
  font-size: 32px;
  line-height: 1.1;
}

.page-header p {
  margin: 8px 0 0;
  color: var(--muted);
  max-width: 64ch;
}

.page-header-actions {
  display: inline-flex;
  gap: 10px;
  align-items: center;
  flex-wrap: wrap;
}

.eyebrow {
  margin: 0 0 8px;
  font-size: 12px;
  letter-spacing: 0.18em;
  text-transform: uppercase;
  color: var(--accent);
  font-weight: 700;
}

.page-body {
  display: grid;
  gap: 18px;
}

.summary-grid {
  display: grid;
  gap: 14px;
  grid-template-columns: repeat(4, minmax(0, 1fr));
}

.card {
  background: var(--panel);
  border: 1px solid var(--border);
  border-radius: 24px;
  box-shadow: var(--shadow);
  backdrop-filter: blur(18px);
}

.stat-card {
  padding: 18px;
  min-height: 140px;
  border-top: 4px solid var(--accent);
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.stat-card[data-tone="blue"] { border-top-color: var(--accent-2); }
.stat-card[data-tone="gold"] { border-top-color: var(--accent-4); }
.stat-card[data-tone="rose"] { border-top-color: var(--accent-3); }
.stat-card[data-tone="teal"] { border-top-color: var(--accent); }

.stat-label {
  text-transform: uppercase;
  letter-spacing: 0.14em;
  font-size: 11px;
  color: var(--muted);
}

.stat-value {
  font-size: 34px;
  font-weight: 800;
  letter-spacing: -0.03em;
}

.stat-hint {
  color: var(--muted);
  line-height: 1.45;
}

.panel {
  padding: 20px;
}

.panel-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 16px;
  margin-bottom: 16px;
}

.panel-head h2,
.panel-head h3 {
  margin: 0;
  font-size: 19px;
}

.panel-head p {
  margin: 6px 0 0;
  color: var(--muted);
}

.panel-toolbar {
  display: flex;
  gap: 10px;
  flex-wrap: wrap;
  align-items: center;
}

.table-shell {
  overflow: auto;
  border-radius: 18px;
  border: 1px solid var(--border);
  background: var(--panel-strong);
}

.table {
  width: 100%;
  min-width: 720px;
  border-collapse: collapse;
}

.table th,
.table td {
  padding: 14px 16px;
  border-bottom: 1px solid rgba(15, 23, 42, 0.06);
  vertical-align: top;
  text-align: left;
}

.table th {
  background: #f7fafc;
  color: var(--muted);
  font-size: 11px;
  text-transform: uppercase;
  letter-spacing: 0.14em;
}

.table tr:last-child td { border-bottom: 0; }

.badge {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 6px 10px;
  border-radius: 999px;
  font-size: 12px;
  font-weight: 700;
  line-height: 1;
}

.badge-muted {
  background: rgba(96, 113, 133, 0.12);
  color: #425066;
}

.badge-success {
  background: rgba(15, 163, 177, 0.12);
  color: #0d6f79;
}

.badge-warning {
  background: rgba(239, 125, 87, 0.12);
  color: #b5532d;
}

.badge-info {
  background: rgba(47, 124, 246, 0.12);
  color: #295dc1;
}

.badge-strong {
  background: rgba(124, 92, 255, 0.12);
  color: #5f41dd;
}

.form-grid {
  display: grid;
  gap: 14px;
  grid-template-columns: repeat(2, minmax(0, 1fr));
}

.field {
  display: grid;
  gap: 8px;
}

.field label {
  font-size: 13px;
  color: var(--muted);
  font-weight: 700;
}

.field input,
.field select,
.field textarea {
  width: 100%;
  border: 1px solid var(--border-strong);
  border-radius: 16px;
  background: #fff;
  color: var(--text);
  padding: 13px 14px;
  outline: none;
}

.field textarea {
  min-height: 120px;
  resize: vertical;
}

.field input:focus,
.field select:focus,
.field textarea:focus {
  border-color: rgba(15, 163, 177, 0.45);
  box-shadow: 0 0 0 4px rgba(15, 163, 177, 0.10);
}

.field .hint {
  color: var(--muted);
  font-size: 12px;
  line-height: 1.45;
}

.section-stack {
  display: grid;
  gap: 18px;
}

.actions {
  display: flex;
  flex-wrap: wrap;
  gap: 10px;
}

.button {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 8px;
  padding: 12px 16px;
  border-radius: 14px;
  border: 1px solid transparent;
  font-weight: 800;
  transition: transform 120ms ease, box-shadow 120ms ease, border-color 120ms ease;
}

.button:hover {
  transform: translateY(-1px);
}

.button.primary {
  color: #fff;
  background: linear-gradient(135deg, var(--accent), var(--accent-2));
  box-shadow: 0 18px 30px rgba(15, 163, 177, 0.18);
}

.button.secondary {
  color: var(--text);
  background: #fff;
  border-color: var(--border);
}

.button.ghost {
  color: var(--muted);
  background: transparent;
  border-color: transparent;
}

.auth-page {
  min-height: 100vh;
  display: grid;
  place-items: center;
  padding: 32px;
}

.auth-panel {
  width: min(980px, 100%);
  padding: 28px;
  border-radius: 30px;
  background: var(--panel);
  border: 1px solid var(--border);
  box-shadow: var(--shadow);
}

.auth-grid {
  display: grid;
  grid-template-columns: minmax(0, 1.1fr) minmax(340px, 0.9fr);
  gap: 24px;
}

.auth-copy {
  padding: 12px 0;
}

.auth-copy h1 {
  margin: 14px 0 12px;
  font-size: 38px;
  line-height: 1.05;
  letter-spacing: -0.04em;
  max-width: 12ch;
}

.auth-copy p {
  margin: 0;
  color: var(--muted);
  max-width: 58ch;
  line-height: 1.7;
}

.feature-list {
  margin: 22px 0 0;
  padding: 0;
  list-style: none;
  display: grid;
  gap: 10px;
}

.feature-list li {
  display: flex;
  align-items: center;
  gap: 10px;
  color: var(--text);
  font-weight: 600;
}

.feature-list li::before {
  content: "";
  width: 10px;
  height: 10px;
  border-radius: 999px;
  background: linear-gradient(135deg, var(--accent), var(--accent-2));
  box-shadow: 0 0 0 5px rgba(15, 163, 177, 0.10);
}

.auth-form {
  padding: 24px;
  border-radius: 24px;
  border: 1px solid var(--border);
  background: rgba(255, 255, 255, 0.76);
  display: grid;
  gap: 16px;
}

.code-block {
  margin: 0;
  padding: 16px;
  border-radius: 18px;
  background: #0f172a;
  color: #e2e8f0;
  overflow: auto;
  font-size: 13px;
  line-height: 1.6;
}

.note {
  padding: 16px 18px;
  border-radius: 18px;
  background: rgba(47, 124, 246, 0.08);
  color: #204893;
  border: 1px solid rgba(47, 124, 246, 0.12);
}

.empty-state {
  padding: 24px;
  border-radius: 18px;
  border: 1px dashed rgba(15, 23, 42, 0.16);
  color: var(--muted);
  background: rgba(255, 255, 255, 0.6);
}

@media (max-width: 1180px) {
  .page-root {
    grid-template-columns: 1fr;
  }

  .sidebar {
    border-right: 0;
    border-bottom: 1px solid rgba(255, 255, 255, 0.06);
  }

  .summary-grid,
  .auth-grid,
  .form-grid {
    grid-template-columns: 1fr;
  }

  .table {
    min-width: 640px;
  }
}
"#;

#[component]
pub fn AppLayout(
    #[prop(into)] title: String,
    #[prop(into)] subtitle: String,
    active: Section,
    #[prop(children)] children: Children,
) -> impl IntoView {
    let page_title = title.clone();
    let page_subtitle = subtitle.clone();
    let nav_items = Section::nav_items();

    view! {
        <!DOCTYPE html>
        <html lang="zh-CN">
          <head>
            <meta charset="utf-8">
            <meta name="viewport" content="width=device-width, initial-scale=1">
            <title>{format!("{page_title} - {APP_NAME}")}</title>
            <style>{APP_CSS}</style>
          </head>
          <body>
            <div class="page-root">
              <aside class="sidebar">
                <div class="brand">
                  <span class="brand-kicker">{APP_NAME}</span>
                  <strong>{page_title.clone()}</strong>
                  <p>{page_subtitle.clone()}</p>
                </div>
                <nav class="nav">
                  {nav_items
                      .into_iter()
                      .map(|item| {
                          let class_name = if item.section == active {
                              "nav-item active"
                          } else {
                              "nav-item"
                          };
                          view! {
                            <a class=class_name href=item.href>
                              <strong>{item.title}</strong>
                              <small>{item.subtitle}</small>
                            </a>
                          }
                      })
                      .collect::<Vec<_>>()}
                </nav>
                <div class="sidebar-footer">
                  <div class="sidebar-footer-card">
                    <strong>SSR-first scaffold</strong>
                    <p class="muted">Leptos 管理后台骨架，后续接入真实 core state 和 session。</p>
                  </div>
                </div>
              </aside>

              <main class="main">
                <header class="page-header">
                  <div>
                    <p class="eyebrow">控制台</p>
                    <h1>{page_title}</h1>
                    <p>{page_subtitle}</p>
                  </div>
                  <div class="page-header-actions">
                    <span class="badge badge-muted">Rust</span>
                    <span class="badge badge-success">SSR</span>
                    <span class="badge badge-info">Leptos</span>
                  </div>
                </header>
                <div class="page-body">
                  {children()}
                </div>
              </main>
            </div>
          </body>
        </html>
    }
}

#[component]
pub fn AuthLayout(
    #[prop(into)] title: String,
    #[prop(into)] subtitle: String,
    #[prop(children)] children: Children,
) -> impl IntoView {
    let page_title = title.clone();
    let page_subtitle = subtitle.clone();

    view! {
        <!DOCTYPE html>
        <html lang="zh-CN">
          <head>
            <meta charset="utf-8">
            <meta name="viewport" content="width=device-width, initial-scale=1">
            <title>{format!("{page_title} - {APP_NAME}")}</title>
            <style>{APP_CSS}</style>
          </head>
          <body>
            <div class="auth-page">
              <div class="auth-panel">
                <div class="auth-grid">
                  <section class="auth-copy">
                    <span class="brand-kicker">{APP_NAME}</span>
                    <h1>{page_title}</h1>
                    <p>{page_subtitle}</p>
                    <ul class="feature-list">
                      <li>管理员会话登录，不再弹出浏览器基础认证</li>
                      <li>SSR 页面，后续可逐步接入核心数据</li>
                      <li>保留协议转换与能力保留的后端边界</li>
                    </ul>
                  </section>
                  <section>
                    {children()}
                  </section>
                </div>
              </div>
            </div>
          </body>
        </html>
    }
}

#[component]
pub fn Panel(
    #[prop(into)] title: String,
    #[prop(into)] subtitle: String,
    #[prop(children)] children: Children,
) -> impl IntoView {
    view! {
        <section class="panel card">
          <div class="panel-head">
            <div>
              <h2>{title}</h2>
              <p>{subtitle}</p>
            </div>
          </div>
          {children()}
        </section>
    }
}

#[component]
pub fn StatCard(
    #[prop(into)] label: String,
    #[prop(into)] value: String,
    #[prop(into)] hint: String,
    tone: Tone,
) -> impl IntoView {
    view! {
      <article class="card stat-card" data-tone=tone.class()>
        <span class="stat-label">{label}</span>
        <strong class="stat-value">{value}</strong>
        <span class="stat-hint">{hint}</span>
      </article>
    }
}

