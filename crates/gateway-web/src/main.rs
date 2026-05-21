use axum::response::{Html, Redirect};
use axum::routing::{get, post};
use axum::Router;
use gateway_web::app::{
    render_dashboard_page, render_downstreams_page, render_login_page, render_logs_page,
    render_portal_page, render_upstreams_page,
};
use std::env;
use std::error::Error;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    init_tracing();

    let bind_addr = env_or("BIND_ADDR", "0.0.0.0:3011");
    let listener = TcpListener::bind(&bind_addr).await?;
    let local_addr = listener.local_addr()?;
    let app = build_router();

    tracing::info!(%bind_addr, %local_addr, "gateway-web listening");
    axum::serve(listener, app.into_make_service()).await?;
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}

fn env_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

fn build_router() -> Router {
    Router::new()
        .route("/", get(root))
        .route("/healthz", get(healthz))
        .route("/admin/login", get(admin_login).post(submit_admin_login))
        .route("/admin/logout", post(admin_logout))
        .route("/admin", get(admin_dashboard))
        .route("/admin/upstreams", get(admin_upstreams).post(submit_upstreams))
        .route("/admin/downstreams", get(admin_downstreams).post(submit_downstreams))
        .route("/admin/logs", get(admin_logs))
        .route("/portal", get(portal))
}

async fn root() -> Redirect {
    Redirect::to("/admin")
}

async fn healthz() -> &'static str {
    "ok"
}

async fn admin_login() -> Html<String> {
    render_login_page()
}

async fn submit_admin_login() -> Redirect {
    Redirect::to("/admin")
}

async fn admin_logout() -> Redirect {
    Redirect::to("/admin/login")
}

async fn admin_dashboard() -> Html<String> {
    render_dashboard_page()
}

async fn admin_upstreams() -> Html<String> {
    render_upstreams_page()
}

async fn submit_upstreams() -> Redirect {
    Redirect::to("/admin/upstreams")
}

async fn admin_downstreams() -> Html<String> {
    render_downstreams_page()
}

async fn submit_downstreams() -> Redirect {
    Redirect::to("/admin/downstreams")
}

async fn admin_logs() -> Html<String> {
    render_logs_page()
}

async fn portal() -> Html<String> {
    render_portal_page()
}
