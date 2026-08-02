//! Optional private upstream concurrency status poller.
//!
//! Polls the non-standard `/dashboard/api/user/request-status` endpoint
//! on each active, opted-in upstream origin. Only `concurrency` and
//! `concurrency_limit` integers are extracted; all billing fields and the
//! raw JSON body are immediately dropped. Failures make the observation
//! unavailable but never disable an upstream or block normal requests.

use crate::keys::upstream_key_fingerprint;
use crate::state::{
    AccountConcurrencyKey, AppState, ProviderConcurrencyObservation,
    ProviderConcurrencyObservationSource, RuntimeCoordinationError, UpstreamConfig,
};
use reqwest::redirect::Policy as RedirectPolicy;
use reqwest::{Client, StatusCode, Url};
use serde::Deserialize;
use std::time::Duration;

/// The fixed private status path appended to each upstream origin.
const STATUS_PATH: &str = "/dashboard/api/user/request-status";

/// Maximum number of same-origin redirects to follow.
const MAX_REDIRECTS: usize = 3;

/// Request timeout for the status probe.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

/// Bounded subset of the status response. Only these two fields are read;
/// everything else (including billing data) is ignored and dropped.
#[derive(Deserialize)]
struct BoundedStatusBody {
    concurrency: i64,
    concurrency_limit: i64,
}

/// Errors that can occur during status observation. They are never logged
/// with response bodies or credentials.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
enum StatusObservationError {
    /// Network error, timeout, or non-200 status.
    Fetch,
    /// Redirect was cross-origin or exceeded the limit.
    Redirect,
    /// Response body was not valid JSON or missing required fields.
    Invalid,
}

impl std::fmt::Display for StatusObservationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fetch => write!(f, "status fetch failed"),
            Self::Redirect => write!(f, "status redirect rejected"),
            Self::Invalid => write!(f, "status response invalid"),
        }
    }
}

impl std::error::Error for StatusObservationError {}

/// Build a reqwest client that follows at most `MAX_REDIRECTS` redirects,
/// only when the target URL has the same scheme, host, and effective port
/// as the original request.
fn build_status_client() -> Client {
    Client::builder()
        .redirect(RedirectPolicy::custom(move |attempt| {
            let previous = attempt.previous();
            if previous.len() > MAX_REDIRECTS {
                return RedirectPolicy::none().redirect(attempt);
            }
            // The first URL in the history is the original request.
            let Some(original) = previous.first() else {
                return RedirectPolicy::none().redirect(attempt);
            };
            let target = attempt.url();
            if !same_origin(original, target) {
                return RedirectPolicy::none().redirect(attempt);
            }
            attempt.follow()
        }))
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(REQUEST_TIMEOUT)
        .build()
        .unwrap_or_else(|error| {
            panic!("failed to build upstream concurrency status client: {error}")
        })
}

/// Check whether two URLs share the same scheme, host, and effective port.
fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
}

/// Extract the origin (scheme://host[:port]) from a base URL.
fn origin_of(base_url: &str) -> Option<String> {
    let url = Url::parse(base_url.trim_end_matches('/')).ok()?;
    let port = url.port_or_known_default();
    match (url.host_str(), port) {
        (Some(host), Some(port)) => Some(format!("{}://{}:{}", url.scheme(), host, port)),
        (Some(host), None) => Some(format!("{}://{}", url.scheme(), host)),
        _ => None,
    }
}

/// Validate a parsed status body and produce a fresh observation.
fn validate_status(
    body: BoundedStatusBody,
    now: u64,
    refresh_seconds: u64,
) -> Result<ProviderConcurrencyObservation, StatusObservationError> {
    let concurrency =
        u32::try_from(body.concurrency).map_err(|_| StatusObservationError::Invalid)?;
    let limit =
        u32::try_from(body.concurrency_limit).map_err(|_| StatusObservationError::Invalid)?;
    if limit == 0 || concurrency > limit {
        return Err(StatusObservationError::Invalid);
    }
    Ok(ProviderConcurrencyObservation {
        source: ProviderConcurrencyObservationSource::PrivateRequestStatus,
        concurrency,
        concurrency_limit: limit,
        observed_at: now,
        fresh_until: now.saturating_add(refresh_seconds),
    })
}

/// Poll a single upstream account for its private concurrency status.
///
/// Returns `Ok(Some(observation))` on success, `Ok(None)` when the upstream
/// is skipped or the response was invalid, and `Err` only on coordination
/// failures (which still do not block traffic).
async fn poll_account(
    state: &AppState,
    client: &Client,
    upstream: &UpstreamConfig,
    api_key: &str,
) -> Result<Option<ProviderConcurrencyObservation>, RuntimeCoordinationError> {
    let upstream_id = &upstream.id;
    let fingerprint = upstream_key_fingerprint(upstream_id, api_key);
    let account = AccountConcurrencyKey::new(upstream_id, &fingerprint);

    // Acquire the poller lease. If another replica owns this interval,
    // skip silently.
    let acquired = state
        .acquire_account_status_poller(&account, state.replica_owner_token())
        .await?;
    if !acquired {
        return Ok(None);
    }

    let origin = match origin_of(&upstream.base_url) {
        Some(origin) => origin,
        None => return Ok(None),
    };
    let url = format!("{origin}{STATUS_PATH}");

    let response = match client
        .get(&url)
        .header("Authorization", format!("Bearer {api_key}"))
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            tracing::debug!(
                upstream_id = %upstream_id,
                error = %error,
                "concurrency status fetch failed"
            );
            return Ok(None);
        }
    };

    let status = response.status();
    if status != StatusCode::OK {
        tracing::debug!(
            upstream_id = %upstream_id,
            status = %status,
            "concurrency status response non-200"
        );
        return Ok(None);
    }

    // Deserialize into a private serde_json::Value, copy only the two
    // named integer fields, and immediately drop the raw value.
    let raw: serde_json::Value = match response.json().await {
        Ok(value) => value,
        Err(error) => {
            tracing::debug!(
                upstream_id = %upstream_id,
                error = %error,
                "concurrency status body parse failed"
            );
            return Ok(None);
        }
    };

    let bounded = match serde_json::from_value::<BoundedStatusBody>(raw.clone()) {
        Ok(body) => body,
        Err(error) => {
            tracing::debug!(
                upstream_id = %upstream_id,
                error = %error,
                "concurrency status fields invalid"
            );
            return Ok(None);
        }
    };

    // Drop the raw value immediately.
    drop(raw);

    let now = crate::util::unix_seconds();
    let refresh = state.config.upstream_concurrency_status_refresh_seconds;
    match validate_status(bounded, now, refresh) {
        Ok(observation) => Ok(Some(observation)),
        Err(error) => {
            tracing::debug!(
                upstream_id = %upstream_id,
                error = %error,
                "concurrency status validation failed"
            );
            Ok(None)
        }
    }
}

/// Collect poll targets from all active, opted-in upstreams.
///
/// Each upstream contributes one or more deduplicated account keys via
/// `UpstreamConfig::account_api_keys()`.
fn collect_targets(upstreams: &[UpstreamConfig]) -> Vec<(&UpstreamConfig, String)> {
    let mut targets = Vec::new();
    let mut seen = std::collections::HashSet::<(String, String)>::new();
    for upstream in upstreams {
        if !upstream.active || !upstream.concurrency_status_enabled {
            continue;
        }
        for api_key in upstream.account_api_keys() {
            let fingerprint = upstream_key_fingerprint(&upstream.id, &api_key);
            let key = (upstream.id.clone(), fingerprint.clone());
            if seen.insert(key) {
                targets.push((upstream, api_key));
            }
        }
    }
    targets
}

/// Poll all eligible upstreams once and store valid observations.
///
/// This function is safe to call concurrently across replicas: the poller
/// lease ensures one hit per account per refresh interval.
pub async fn poll_concurrency_status_once(state: &AppState) {
    let upstreams = state.upstreams().await;
    let targets = collect_targets(&upstreams);
    if targets.is_empty() {
        return;
    }
    let client = build_status_client();
    for (upstream, api_key) in targets {
        match poll_account(state, &client, upstream, &api_key).await {
            Ok(Some(observation)) => {
                if let Err(error) = state
                    .store_provider_concurrency_observation(
                        &AccountConcurrencyKey::new(
                            upstream.id.as_str(),
                            upstream_key_fingerprint(&upstream.id, &api_key).as_str(),
                        ),
                        observation.concurrency,
                        observation.concurrency_limit,
                    )
                    .await
                {
                    tracing::debug!(
                        upstream_id = %upstream.id,
                        error = %error,
                        "failed to store concurrency observation"
                    );
                }
            }
            Ok(None) => {}
            Err(error) => {
                tracing::debug!(
                    upstream_id = %upstream.id,
                    error = %error,
                    "concurrency status poller coordination failed"
                );
            }
        }
    }
}

/// Spawn the background poller that runs every `refresh_seconds`.
pub fn spawn_concurrency_status_poller(state: AppState) -> tokio::task::JoinHandle<()> {
    let interval = Duration::from_secs(
        state
            .config
            .upstream_concurrency_status_refresh_seconds
            .max(1),
    );
    tokio::spawn(async move {
        // Initial short delay to let startup settle.
        tokio::time::sleep(Duration::from_secs(2)).await;
        loop {
            poll_concurrency_status_once(&state).await;
            tokio::time::sleep(interval).await;
        }
    })
}
