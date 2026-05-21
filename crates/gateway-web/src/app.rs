use axum::response::Html;
use leptos::prelude::*;
use leptos::ssr::render_to_string;

use crate::pages::{
    dashboard::DashboardPage, downstreams::DownstreamsPage, login::LoginPage, logs::LogsPage,
    portal::PortalPage, upstreams::UpstreamsPage,
};

fn render_view<F, N>(view: F) -> Html<String>
where
    F: FnOnce() -> N + 'static,
    N: IntoView,
{
    Html(render_to_string(view))
}

pub fn render_login_page() -> Html<String> {
    render_view(LoginPage)
}

pub fn render_dashboard_page() -> Html<String> {
    render_view(DashboardPage)
}

pub fn render_upstreams_page() -> Html<String> {
    render_view(UpstreamsPage)
}

pub fn render_downstreams_page() -> Html<String> {
    render_view(DownstreamsPage)
}

pub fn render_logs_page() -> Html<String> {
    render_view(LogsPage)
}

pub fn render_portal_page() -> Html<String> {
    render_view(PortalPage)
}

