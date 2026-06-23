use base64::Engine;
use reqwest::Url;
use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

use crate::state::AppConfig;

/// Current Unix timestamp (seconds).
pub fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Generate a new prefixed UUID.
pub fn new_id(prefix: &str) -> String {
    format!("{}_{}", prefix, Uuid::new_v4())
}

/// Base64url-encode bytes without padding.
pub fn encode_secret_suffix(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Join a base URL with an endpoint path, avoiding double slashes and
/// duplicate version segments.
pub fn join_upstream_url(base_url: &str, endpoint_path: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let path = endpoint_path.trim_start_matches('/');

    if let Some((version, remainder)) = path.split_once('/') {
        if base.ends_with(&format!("/{version}")) {
            return format!("{base}/{}", remainder);
        }
    }

    format!("{base}/{}", path)
}

/// Whether an HTTP request to the given URL should bypass the system proxy.
pub fn should_bypass_proxy_for_url(url: &str) -> bool {
    Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(should_bypass_proxy_for_host))
        .unwrap_or(false)
}

/// Whether the given hostname should bypass the system proxy.
pub fn should_bypass_proxy_for_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
}

/// Build an HTTP client for upstream requests.
pub fn build_upstream_http_client(config: &AppConfig, no_proxy: bool) -> reqwest::Client {
    let mut builder = reqwest::Client::builder().connect_timeout(Duration::from_secs(
        config.upstream_connect_timeout_seconds.max(1),
    ));
    builder = builder.pool_max_idle_per_host(config.upstream_http_pool_max_idle_per_host);
    if no_proxy {
        builder = builder.no_proxy();
    }

    builder.build().unwrap_or_else(|error| {
        tracing::warn!(%error, no_proxy, "failed to build upstream HTTP client, falling back");
        reqwest::Client::new()
    })
}

/// Remove expired admin sessions.
pub fn prune_expired_admin_sessions(sessions: &mut HashMap<String, u64>) {
    let now = unix_seconds();
    sessions.retain(|_, expires_at| *expires_at > now);
}
