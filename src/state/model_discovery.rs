use futures_util::{stream, StreamExt};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

pub const MODEL_DISCOVERY_MAX_CONCURRENCY: usize = 8;

#[derive(Debug, Clone)]
pub struct KeyModelDiscoveryResult {
    pub key_index: usize,
    pub models: Vec<String>,
    pub latency_ms: u64,
    pub error: Option<String>,
}

/// Keep the legacy model-probe channel label generation in one place while the
/// discovery result itself remains index-only and cannot carry a raw Key.
pub fn model_discovery_key_prefix(key: &str) -> String {
    let key = key.trim();
    let prefix = key.chars().take(8).collect::<String>();
    if key.chars().count() <= 8 {
        prefix
    } else {
        format!("{}...", prefix)
    }
}

pub async fn fetch_models_from_upstream(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    timeout_seconds: u64,
) -> Result<Vec<String>, String> {
    let url = crate::util::join_upstream_url(base_url, "/v1/models");
    let response = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .timeout(Duration::from_secs(timeout_seconds.max(1)))
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                "upstream model discovery timed out".to_string()
            } else if error.is_connect() {
                "upstream model discovery connection failed".to_string()
            } else {
                "upstream model discovery request failed".to_string()
            }
        })?;

    let status = response.status();
    if !status.is_success() {
        // Do not read or expose the provider body. It may contain credentials,
        // request data, or an unbounded diagnostic payload.
        return Err(format!(
            "upstream model discovery returned status {}",
            status.as_u16()
        ));
    }

    let payload: Value = response
        .json()
        .await
        .map_err(|_| "upstream model discovery returned invalid JSON".to_string())?;

    let data = payload
        .get("data")
        .and_then(|value| value.as_array())
        .ok_or_else(|| "upstream model discovery response missing data".to_string())?;

    let mut models: Vec<String> = data
        .iter()
        .filter_map(|item| item.get("id").and_then(|value| value.as_str()))
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty())
        .collect();

    models.sort();
    models.dedup();

    if models.is_empty() {
        return Err("upstream returned no models".to_string());
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
                return (key_indices, Vec::new(), 0, Some("key is empty".to_string()));
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
            results.push(KeyModelDiscoveryResult {
                key_index,
                models: models.clone(),
                latency_ms,
                error: error.clone(),
            });
        }
    }
    results.sort_by_key(|result| result.key_index);
    results
}
