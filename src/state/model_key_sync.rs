use super::freekey_sync::derive_supported_models;
use super::model_discovery::{
    fetch_models_from_upstream_keys_concurrently, KeyModelDiscoveryResult,
};
use super::{join_upstream_url, unix_seconds, ApiKeyModelConfig, AppState, UpstreamConfig};
use std::collections::{HashMap, HashSet};
use std::io;
use std::time::Duration;
use tokio::time::{interval, MissedTickBehavior};

#[derive(Debug, Default, Clone)]
pub struct ModelKeySyncSummary {
    pub upstreams_scanned: usize,
    pub upstreams_updated: usize,
    pub upstreams_unchanged: usize,
    pub skipped: usize,
    pub keys_succeeded: usize,
    pub keys_failed: usize,
}

#[derive(Debug, Clone)]
struct UpstreamKeySyncSnapshot {
    upstream_id: String,
    base_url: String,
    keys: Vec<String>,
    results: Vec<KeyModelDiscoveryResult>,
}

fn same_key_set(left: &[String], right: &[String]) -> bool {
    let left = left
        .iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<HashSet<_>>();
    let right = right
        .iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<HashSet<_>>();
    left == right
}

fn apply_key_model_refresh(
    upstream: &mut UpstreamConfig,
    results: &[KeyModelDiscoveryResult],
    synced_at: u64,
) -> bool {
    let mut refreshed_key_models: HashMap<String, ApiKeyModelConfig> = upstream
        .api_key_models
        .iter()
        .cloned()
        .map(|mapping| (mapping.api_key.clone(), mapping))
        .collect();
    let mut any_success = false;

    for result in results {
        if result.error.is_some() {
            continue;
        }
        any_success = true;
        refreshed_key_models.insert(
            result.key.clone(),
            ApiKeyModelConfig {
                api_key: result.key.clone(),
                supported_models: result.models.clone(),
            },
        );
    }

    if !any_success {
        return false;
    }

    let ordered_keys = upstream.available_keys();
    let mut updated_api_key_models = Vec::new();
    let mut seen = HashSet::new();

    for key in ordered_keys {
        if !seen.insert(key.clone()) {
            continue;
        }
        if let Some(mapping) = refreshed_key_models.get(&key) {
            updated_api_key_models.push(mapping.clone());
        }
    }

    for (key, mapping) in refreshed_key_models {
        if seen.insert(key) {
            updated_api_key_models.push(mapping);
        }
    }

    upstream.api_key_models = updated_api_key_models;
    upstream.supported_models = derive_supported_models(&upstream.api_key_models);
    upstream.last_synced_at = synced_at;
    upstream.normalize_for_storage();
    true
}

impl AppState {
    pub async fn sync_upstream_model_key_mappings(&self) -> io::Result<ModelKeySyncSummary> {
        let snapshot = self.snapshot().await;
        let timeout_seconds = self.config.admin_upstream_timeout_seconds.max(1);
        let synced_at = unix_seconds();
        let mut discoveries: Vec<UpstreamKeySyncSnapshot> = Vec::new();

        for upstream in snapshot.upstreams.iter().filter(|upstream| upstream.active) {
            let keys = upstream.available_keys();
            if keys.is_empty() {
                continue;
            }

            let probe_url = join_upstream_url(&upstream.base_url, "/v1/models");
            let client = self.client_for_url(&probe_url);
            let results = fetch_models_from_upstream_keys_concurrently(
                &client,
                &upstream.base_url,
                &keys,
                timeout_seconds,
            )
            .await;

            discoveries.push(UpstreamKeySyncSnapshot {
                upstream_id: upstream.id.clone(),
                base_url: upstream.base_url.clone(),
                keys,
                results,
            });
        }

        self.mutate_persisted_state_io(|candidate_state| {
            let mut summary = ModelKeySyncSummary::default();

            for discovery in discoveries {
                summary.upstreams_scanned = summary.upstreams_scanned.saturating_add(1);
                summary.keys_succeeded = summary.keys_succeeded.saturating_add(
                    discovery
                        .results
                        .iter()
                        .filter(|result| result.error.is_none())
                        .count(),
                );
                summary.keys_failed = summary.keys_failed.saturating_add(
                    discovery
                        .results
                        .iter()
                        .filter(|result| result.error.is_some())
                        .count(),
                );

                let Some(upstream) = candidate_state
                    .upstreams
                    .iter_mut()
                    .find(|upstream| upstream.id == discovery.upstream_id)
                else {
                    summary.skipped = summary.skipped.saturating_add(1);
                    continue;
                };

                if !upstream.active
                    || upstream.base_url != discovery.base_url
                    || !same_key_set(&upstream.available_keys(), &discovery.keys)
                {
                    summary.skipped = summary.skipped.saturating_add(1);
                    continue;
                }

                if apply_key_model_refresh(upstream, &discovery.results, synced_at) {
                    summary.upstreams_updated = summary.upstreams_updated.saturating_add(1);
                } else {
                    summary.upstreams_unchanged = summary.upstreams_unchanged.saturating_add(1);
                }
            }

            Ok(summary)
        })
        .await
    }

    pub async fn run_model_key_sync_loop(self) {
        let interval_seconds = self.config.upstream_model_key_sync_interval_seconds.max(1);
        let mut ticker = interval(Duration::from_secs(interval_seconds));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            ticker.tick().await;
            match self.sync_upstream_model_key_mappings().await {
                Ok(summary) => {
                    tracing::info!(
                        scanned = summary.upstreams_scanned,
                        updated = summary.upstreams_updated,
                        unchanged = summary.upstreams_unchanged,
                        skipped = summary.skipped,
                        keys_succeeded = summary.keys_succeeded,
                        keys_failed = summary.keys_failed,
                        "model-key sync cycle completed"
                    );
                }
                Err(error) => {
                    tracing::warn!(error = %error, "model-key sync cycle failed");
                }
            }
        }
    }
}
