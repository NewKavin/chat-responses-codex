use futures_util::{stream, StreamExt};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

pub const MODEL_DISCOVERY_MAX_CONCURRENCY: usize = 8;

pub fn model_discovery_url(base_url: &str) -> String {
    crate::util::join_upstream_url(base_url, "/v1/models")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelDiscoveryError {
    Timeout,
    Connection,
    Request,
    HttpStatus(u16),
    InvalidJson,
    MissingData,
    EmptyModels,
}

impl ModelDiscoveryError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Connection => "connection",
            Self::Request => "request",
            Self::HttpStatus(_) => "http_status",
            Self::InvalidJson => "invalid_json",
            Self::MissingData => "missing_data",
            Self::EmptyModels => "empty_models",
        }
    }

    pub fn http_status(&self) -> Option<u16> {
        match self {
            Self::HttpStatus(status) => Some(*status),
            _ => None,
        }
    }
}

impl fmt::Display for ModelDiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout => formatter.write_str("upstream model discovery timed out"),
            Self::Connection => formatter.write_str("upstream model discovery connection failed"),
            Self::Request => formatter.write_str("upstream model discovery request failed"),
            Self::HttpStatus(status) => {
                write!(
                    formatter,
                    "upstream model discovery returned status {status}"
                )
            }
            Self::InvalidJson => {
                formatter.write_str("upstream model discovery returned invalid JSON")
            }
            Self::MissingData => {
                formatter.write_str("upstream model discovery response missing data")
            }
            Self::EmptyModels => formatter.write_str("upstream returned no models"),
        }
    }
}

impl std::error::Error for ModelDiscoveryError {}

#[derive(Debug, Clone)]
pub struct KeyModelDiscoveryResult {
    pub key_index: usize,
    pub models: Vec<String>,
    pub latency_ms: u64,
    pub error: Option<String>,
    pub error_code: Option<String>,
    pub http_status: Option<u16>,
}

pub async fn fetch_models_from_upstream(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    timeout_seconds: u64,
) -> Result<Vec<String>, ModelDiscoveryError> {
    let url = model_discovery_url(base_url);
    let response = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .timeout(Duration::from_secs(timeout_seconds.max(1)))
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                ModelDiscoveryError::Timeout
            } else if error.is_connect() {
                ModelDiscoveryError::Connection
            } else {
                ModelDiscoveryError::Request
            }
        })?;

    let status = response.status();
    if !status.is_success() {
        // Do not read or expose the provider body. It may contain credentials,
        // request data, or an unbounded diagnostic payload.
        return Err(ModelDiscoveryError::HttpStatus(status.as_u16()));
    }

    let payload: Value = response
        .json()
        .await
        .map_err(|_| ModelDiscoveryError::InvalidJson)?;

    let data = payload
        .get("data")
        .and_then(|value| value.as_array())
        .ok_or(ModelDiscoveryError::MissingData)?;

    let mut models: Vec<String> = data
        .iter()
        .filter_map(|item| item.get("id").and_then(|value| value.as_str()))
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty())
        .collect();

    models.sort();
    models.dedup();

    if models.is_empty() {
        return Err(ModelDiscoveryError::EmptyModels);
    }

    Ok(models)
}

pub async fn fetch_models_from_upstream_keys_concurrently(
    client: &reqwest::Client,
    base_url: &str,
    keys: &[String],
    timeout_seconds: u64,
) -> Vec<KeyModelDiscoveryResult> {
    if keys.is_empty() {
        return Vec::new();
    }

    let base_url = base_url.trim().to_string();
    let mut unique_keys: Vec<(String, Vec<usize>)> = Vec::new();
    let mut positions = HashMap::<String, usize>::new();
    for (key_index, key) in keys.iter().enumerate() {
        let normalized = key.trim().to_string();
        if let Some(position) = positions.get(&normalized).copied() {
            unique_keys[position].1.push(key_index);
        } else {
            positions.insert(normalized.clone(), unique_keys.len());
            unique_keys.push((normalized, vec![key_index]));
        }
    }

    let concurrency = unique_keys.len().clamp(1, MODEL_DISCOVERY_MAX_CONCURRENCY);
    let shared_results = stream::iter(unique_keys.into_iter().map(|(key, key_indices)| {
        let client = client.clone();
        let base_url = base_url.clone();
        async move {
            if key.is_empty() {
                return (
                    key_indices,
                    Vec::new(),
                    0,
                    Some(ModelDiscoveryError::Request),
                );
            }

            let started = std::time::Instant::now();
            match fetch_models_from_upstream(&client, &base_url, &key, timeout_seconds).await {
                Ok(models) => (
                    key_indices,
                    models,
                    started.elapsed().as_millis().max(1) as u64,
                    None,
                ),
                Err(error) => (
                    key_indices,
                    Vec::new(),
                    started.elapsed().as_millis().max(1) as u64,
                    Some(error),
                ),
            }
        }
    }))
    .buffer_unordered(concurrency)
    .collect::<Vec<_>>()
    .await;

    let mut results = Vec::with_capacity(keys.len());
    for (key_indices, models, latency_ms, error) in shared_results {
        for key_index in key_indices {
            let (error_message, error_code, http_status) =
                error.as_ref().map_or((None, None, None), |error| {
                    (
                        Some(error.to_string()),
                        Some(error.code().to_string()),
                        error.http_status(),
                    )
                });
            results.push(KeyModelDiscoveryResult {
                key_index,
                models: models.clone(),
                latency_ms,
                error: error_message,
                error_code,
                http_status,
            });
        }
    }
    results.sort_by_key(|result| result.key_index);
    results
}
