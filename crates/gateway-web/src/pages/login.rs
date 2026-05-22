use leptos::prelude::*;

use gateway_core::state::AppConfig;

use crate::shell::AuthLayout;

#[component]
pub fn LoginPage(config: AppConfig) -> impl IntoView {
    let app_name = config.app_name.clone();
    let admin_username = config.admin_username.clone();

    view! {
        <AuthLayout
            title="管理员登录"
            subtitle=format!(
                "{app_name} 的管理员会话入口。登录后可以管理上游、下游、日志和门户，网关核心逻辑仍留在后端。"
            )
        >
            <form class="auth-form" method="post" action="/admin/login">
              <div class="field">
                <label for="username">用户名</label>
                <input id="username" name="username" value=admin_username autocomplete="username" />
              </div>
              <div class="field">
                <label for="password">密码</label>
                <input id="password" name="password" type="password" autocomplete="current-password" />
              </div>
              <div class="actions">
                <button class="button primary" type="submit">进入控制台</button>
                <a class="button secondary" href="/portal">查看门户</a>
              </div>
              <p class="note">{"这里直接读取核心 AppConfig，管理员用户名会按后端配置预填，后续只替换成真实 session 即可。"}</p>
            </form>
        </AuthLayout>
    }
}
