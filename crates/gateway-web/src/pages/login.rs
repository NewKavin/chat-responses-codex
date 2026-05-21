use leptos::prelude::*;

use crate::shell::AuthLayout;

#[component]
pub fn LoginPage() -> impl IntoView {
    view! {
        <AuthLayout
            title="管理员登录"
            subtitle="使用管理员会话进入控制台，管理上游、下游、日志和门户。"
        >
            <form class="auth-form" method="post" action="/admin/login">
              <div class="field">
                <label for="username">用户名</label>
                <input id="username" name="username" value="admin" autocomplete="username">
              </div>
              <div class="field">
                <label for="password">密码</label>
                <input id="password" name="password" type="password" autocomplete="current-password">
              </div>
              <div class="actions">
                <button class="button primary" type="submit">进入控制台</button>
                <a class="button secondary" href="/portal">查看门户</a>
              </div>
              <p class="note">这个 scaffold 先保持 SSR-first，后续再接入真实 session 和鉴权状态。</p>
            </form>
        </AuthLayout>
    }
}

