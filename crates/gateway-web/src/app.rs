use axum::response::Html;
use leptos::prelude::*;
use leptos::tachys::view::RenderHtml;

use crate::demo::{
    dashboard_context, downstreams_context, login_config, logs_context, portal_context,
    upstreams_context,
};
use crate::pages::logs::LogListQuery;
use crate::pages::{
    dashboard::DashboardPage, downstreams::DownstreamsPage, login::LoginPage, logs::LogsPage,
    portal::PortalPage, upstreams::UpstreamsPage,
};

fn render_view<F, N>(view: F) -> Html<String>
where
    F: FnOnce() -> N + 'static,
    N: IntoView,
{
    Html(view().into_view().to_html())
}

pub fn render_login_page() -> Html<String> {
    let config = login_config();
    render_view(move || view! { <LoginPage config=config /> })
}

pub fn render_dashboard_page() -> Html<String> {
    let (config, state) = dashboard_context();
    render_view(move || view! { <DashboardPage config=config state=state /> })
}

pub fn render_upstreams_page(edit_id: Option<&str>) -> Html<String> {
    let (config, state, form, notice, form_open) = upstreams_context(edit_id);
    render_view(move || {
        view! { <UpstreamsPage config=config state=state form=form notice=notice form_open=form_open /> }
    })
}

pub fn render_downstreams_page(
    query: gateway_core::admin::DownstreamListQuery,
    edit_id: Option<&str>,
) -> Html<String> {
    let (config, state, form, query, notice, form_open) = downstreams_context(edit_id, query);
    render_view(move || {
        view! { <DownstreamsPage config=config state=state form=form query=query notice=notice form_open=form_open /> }
    })
}

pub fn render_logs_page() -> Html<String> {
    render_logs_page_with_query(LogListQuery::default())
}

pub fn render_logs_page_with_query(query: LogListQuery) -> Html<String> {
    let (config, state) = logs_context();
    render_view(move || view! { <LogsPage config=config state=state query=query /> })
}

pub fn render_portal_page() -> Html<String> {
    let (config, state) = portal_context();
    render_view(move || view! { <PortalPage config=config state=state /> })
}
