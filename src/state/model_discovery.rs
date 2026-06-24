use futures_util::{stream, StreamExt};
use serde_json::Value;
use std::time::Duration;

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct KeyModelDiscoveryResult {
    pub index: usize,
    pub key: String,
    pub key_prefix: String,
    pub models: Vec<String>,
    pub latency_ms: u64,
    pub error: Option<String>,
}

fn key_prefix(key: &str) -> String {
    let key = key.trim();
    if key.len() <= 8 {
        key.to_string()
    } else {
        format!("{}...", &key[..8])
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
        .map_err(|e| format!("请求 {} 失败: {}", url, e))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(format!("{} 返回 {}: {}", url, status.as_u16(), body));
    }

    let payload: Value = response
        .json()
        .await
        .map_err(|e| format!("解析 {} 响应失败: {}", url, e))?;

    let data = payload
        .get("data")
        .and_then(|v| v.as_array())
        .ok_or_else(|| format!("{} 响应缺少 data 字段", url))?;

    let mut models: Vec<String> = data
        .iter()
        .filter_map(|item| item.get("id").and_then(|v| v.as_str()))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    models.sort();
    models.dedup();

    if models.is_empty() {
        return Err(format!("{} 未返回任何模型", url));
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
    let concurrency = keys.len().max(1);
    let mut results = stream::iter(keys.iter().cloned().enumerate().map(|(index, key)| {
        let client = client.clone();
        let base_url = base_url.clone();
        let key = key.trim().to_string();
        let key_prefix = key_prefix(&key);

        async move {
            if key.is_empty() {
                return KeyModelDiscoveryResult {
                    index,
                    key,
                    key_prefix,
                    models: Vec::new(),
                    latency_ms: 0,
                    error: Some("key 为空".to_string()),
                };
            }

            let started = std::time::Instant::now();
            match fetch_models_from_upstream(&client, &base_url, &key, timeout_seconds).await {
                Ok(models) => KeyModelDiscoveryResult {
                    index,
                    key,
                    key_prefix,
                    models,
                    latency_ms: started.elapsed().as_millis().max(1) as u64,
                    error: None,
                },
                Err(error) => KeyModelDiscoveryResult {
                    index,
                    key,
                    key_prefix,
                    models: Vec::new(),
                    latency_ms: started.elapsed().as_millis().max(1) as u64,
                    error: Some(error),
                },
            }
        }
    }))
    .buffer_unordered(concurrency)
    .collect::<Vec<_>>()
    .await;

    results.sort_by_key(|item| item.index);
    results
}
