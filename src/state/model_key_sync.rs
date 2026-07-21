use super::freekey_sync::derive_supported_models;
use super::model_discovery::{
    fetch_models_from_upstream, fetch_models_from_upstream_keys_concurrently,
    KeyModelDiscoveryResult,
};
use super::{unix_seconds, ApiKeyModelConfig, AppState, RouteHealthKey, UpstreamConfig};
use crate::capabilities::WireProtocol;
use crate::keys::upstream_key_fingerprint;
use crate::routing::UpstreamProtocol;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io;
use std::time::Duration;
use tokio::time::Instant;
use tokio::{sync::mpsc, task::JoinHandle};

const MISSING_MODEL_CONFIRMATION_INTERVAL: Duration = Duration::from_secs(60);
pub const TARGETED_DISCOVERY_QUEUE_CAPACITY: usize = 256;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct MissingObservationKey {
    upstream_id: String,
    key_fingerprint: String,
    model: String,
}

#[derive(Clone, Debug)]
struct MissingObservation {
    count: u8,
    last_successful_missing_at: Instant,
    configuration_fingerprint: String,
}

#[derive(Default)]
pub(super) struct ModelKeySyncRuntime {
    missing_observations: HashMap<MissingObservationKey, MissingObservation>,
    targeted_sender: Option<mpsc::Sender<TargetedModelDiscoveryJob>>,
    targeted_pending: HashSet<TargetedDiscoveryKey>,
    periodic_cycles_started: u64,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct TargetedDiscoveryKey {
    upstream_id: String,
    key_fingerprint: String,
}

#[derive(Clone, Debug)]
struct TargetedModelDiscoveryJob {
    key: TargetedDiscoveryKey,
    target_model: String,
}

pub struct ModelKeySyncService;

struct TargetedPendingGuard {
    state: AppState,
    key: TargetedDiscoveryKey,
}

struct ModelKeySyncServiceGuard {
    state: AppState,
}

impl Drop for TargetedPendingGuard {
    fn drop(&mut self) {
        self.state
            .model_key_sync_runtime
            .lock()
            .expect("model key sync runtime lock poisoned")
            .targeted_pending
            .remove(&self.key);
    }
}

impl Drop for ModelKeySyncServiceGuard {
    fn drop(&mut self) {
        let mut runtime = self
            .state
            .model_key_sync_runtime
            .lock()
            .expect("model key sync runtime lock poisoned");
        runtime.targeted_sender = None;
        runtime.targeted_pending.clear();
    }
}

#[derive(Debug, Default, Clone, Eq, PartialEq)]
pub struct ModelKeySyncSummary {
    pub upstreams_scanned: usize,
    pub upstreams_updated: usize,
    pub upstreams_unchanged: usize,
    pub skipped: usize,
    pub keys_succeeded: usize,
    pub keys_failed: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ModelKeySyncSnapshot {
    upstream_id: String,
    base_url: String,
    ordered_current_keys: Vec<(String, String)>,
    protocols: Vec<UpstreamProtocol>,
    api_key_models: Vec<ApiKeyModelConfig>,
    supported_models: Vec<String>,
    configuration_fingerprint: String,
}

impl ModelKeySyncSnapshot {
    fn capture(upstream: &UpstreamConfig) -> Self {
        let ordered_current_keys = upstream
            .available_keys()
            .into_iter()
            .map(|key| {
                let fingerprint = upstream_key_fingerprint(&upstream.id, &key);
                (key, fingerprint)
            })
            .collect();
        Self {
            upstream_id: upstream.id.clone(),
            base_url: upstream.base_url.clone(),
            ordered_current_keys,
            protocols: upstream.supported_protocols(),
            api_key_models: upstream.api_key_models.clone(),
            supported_models: upstream.supported_models.clone(),
            configuration_fingerprint: sync_configuration_fingerprint(upstream),
        }
    }

    fn matches(&self, upstream: &UpstreamConfig) -> bool {
        self == &Self::capture(upstream)
    }

    fn raw_keys(&self) -> Vec<String> {
        self.ordered_current_keys
            .iter()
            .map(|(key, _)| key.clone())
            .collect()
    }
}

struct CompletedDiscovery {
    snapshot: ModelKeySyncSnapshot,
    results: Vec<KeyModelDiscoveryResult>,
}

fn sync_configuration_fingerprint(upstream: &UpstreamConfig) -> String {
    let mut stable = upstream.clone();
    stable.failure_count = 0;
    stable.last_synced_at = 0;
    let bytes = serde_json::to_vec(&stable).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(b"chat2responses:model-key-sync:v1\0");
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn deterministic_jitter(seed: &str, minimum: u64, spread: u64) -> Duration {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in seed.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    Duration::from_secs(minimum.saturating_add(hash % spread.max(1)))
}

fn apply_discovery(
    upstream: &mut UpstreamConfig,
    snapshot: &ModelKeySyncSnapshot,
    results: &[KeyModelDiscoveryResult],
    synced_at: u64,
    now: Instant,
    missing_observations: &mut HashMap<MissingObservationKey, MissingObservation>,
) -> bool {
    let successful = results
        .iter()
        .filter(|result| result.error.is_none())
        .count();
    if successful == 0 {
        return false;
    }

    let keys = upstream.available_keys();
    if upstream.api_key_models.is_empty() && successful != keys.len() {
        return false;
    }

    let existing = upstream
        .api_key_models
        .iter()
        .cloned()
        .map(|mapping| (mapping.api_key.clone(), mapping.supported_models))
        .collect::<HashMap<_, _>>();
    let discovered = results
        .iter()
        .map(|result| (result.key_index, result))
        .collect::<HashMap<_, _>>();
    let mut mappings = Vec::with_capacity(keys.len());
    for (key_index, key) in keys.into_iter().enumerate() {
        let key_fingerprint = upstream_key_fingerprint(&upstream.id, &key);
        let old_models = existing.get(&key).cloned().unwrap_or_default();
        let models = if let Some(result) = discovered
            .get(&key_index)
            .filter(|result| result.error.is_none())
        {
            let discovered_models = result.models.iter().cloned().collect::<HashSet<_>>();
            for model in &result.models {
                missing_observations.remove(&MissingObservationKey {
                    upstream_id: upstream.id.clone(),
                    key_fingerprint: key_fingerprint.clone(),
                    model: model.clone(),
                });
            }
            let mut models = result.models.clone();
            for model in old_models {
                if discovered_models.contains(&model) {
                    continue;
                }
                let observation_key = MissingObservationKey {
                    upstream_id: upstream.id.clone(),
                    key_fingerprint: key_fingerprint.clone(),
                    model: model.clone(),
                };
                let confirmed = match missing_observations.get_mut(&observation_key) {
                    Some(observation)
                        if observation.configuration_fingerprint
                            == snapshot.configuration_fingerprint =>
                    {
                        if now.duration_since(observation.last_successful_missing_at)
                            >= MISSING_MODEL_CONFIRMATION_INTERVAL
                        {
                            observation.count = observation.count.saturating_add(1);
                            observation.last_successful_missing_at = now;
                        }
                        observation.count >= 2
                    }
                    _ => {
                        missing_observations.insert(
                            observation_key.clone(),
                            MissingObservation {
                                count: 1,
                                last_successful_missing_at: now,
                                configuration_fingerprint: snapshot
                                    .configuration_fingerprint
                                    .clone(),
                            },
                        );
                        false
                    }
                };
                if confirmed {
                    missing_observations.remove(&observation_key);
                } else {
                    models.push(model);
                }
            }
            models
        } else {
            old_models
        };
        mappings.push(ApiKeyModelConfig {
            api_key: key,
            supported_models: models,
        });
    }

    upstream.api_key_models = mappings;
    upstream.supported_models = derive_supported_models(&upstream.api_key_models);
    upstream.last_synced_at = synced_at;
    upstream.normalize_for_storage();
    let next_configuration_fingerprint = sync_configuration_fingerprint(upstream);
    for (key, observation) in missing_observations.iter_mut() {
        if key.upstream_id == upstream.id
            && observation.configuration_fingerprint == snapshot.configuration_fingerprint
        {
            observation.configuration_fingerprint = next_configuration_fingerprint.clone();
        }
    }
    true
}

impl AppState {
    pub(super) fn reconcile_model_key_sync_runtime(&self, upstreams: &[UpstreamConfig]) {
        let valid_keys = upstreams
            .iter()
            .filter(|upstream| upstream.active)
            .flat_map(|upstream| {
                upstream
                    .available_keys()
                    .into_iter()
                    .map(|api_key| TargetedDiscoveryKey {
                        upstream_id: upstream.id.clone(),
                        key_fingerprint: upstream_key_fingerprint(&upstream.id, &api_key),
                    })
            })
            .collect::<HashSet<_>>();
        let mut runtime = self
            .model_key_sync_runtime
            .lock()
            .expect("model key sync runtime lock poisoned");
        runtime
            .targeted_pending
            .retain(|key| valid_keys.contains(key));
        runtime.missing_observations.retain(|key, _| {
            valid_keys.contains(&TargetedDiscoveryKey {
                upstream_id: key.upstream_id.clone(),
                key_fingerprint: key.key_fingerprint.clone(),
            })
        });
    }

    pub fn submit_targeted_model_discovery(
        &self,
        upstream_id: &str,
        key_fingerprint: &str,
        target_model: &str,
    ) -> bool {
        let key = TargetedDiscoveryKey {
            upstream_id: upstream_id.to_string(),
            key_fingerprint: key_fingerprint.to_string(),
        };
        let mut runtime = self
            .model_key_sync_runtime
            .lock()
            .expect("model key sync runtime lock poisoned");
        let Some(sender) = runtime.targeted_sender.clone() else {
            return false;
        };
        if runtime.targeted_pending.len() >= TARGETED_DISCOVERY_QUEUE_CAPACITY {
            return false;
        }
        if !runtime.targeted_pending.insert(key.clone()) {
            return false;
        }
        let job = TargetedModelDiscoveryJob {
            key: key.clone(),
            target_model: target_model.to_string(),
        };
        if sender.try_send(job).is_err() {
            runtime.targeted_pending.remove(&key);
            return false;
        }
        true
    }

    pub fn targeted_model_discovery_pending_count(&self) -> usize {
        self.model_key_sync_runtime
            .lock()
            .expect("model key sync runtime lock poisoned")
            .targeted_pending
            .len()
    }

    pub fn periodic_model_sync_cycle_count(&self) -> u64 {
        self.model_key_sync_runtime
            .lock()
            .expect("model key sync runtime lock poisoned")
            .periodic_cycles_started
    }

    async fn process_targeted_model_discovery(&self, job: TargetedModelDiscoveryJob) {
        let _pending = TargetedPendingGuard {
            state: self.clone(),
            key: job.key.clone(),
        };
        let _sync_guard = self.model_key_sync_lock.lock().await;
        let routing = self.routing_snapshot().await;
        let Some(upstream) = routing.upstreams.iter().find(|upstream| {
            upstream.active
                && upstream.id == job.key.upstream_id
                && upstream.available_keys().iter().any(|api_key| {
                    upstream_key_fingerprint(&upstream.id, api_key) == job.key.key_fingerprint
                })
        }) else {
            return;
        };
        let snapshot = ModelKeySyncSnapshot::capture(upstream);
        let Some(api_key) = upstream.available_keys().into_iter().find(|api_key| {
            upstream_key_fingerprint(&upstream.id, api_key) == job.key.key_fingerprint
        }) else {
            return;
        };
        let url = crate::util::join_upstream_url(&upstream.base_url, "/v1/models");
        let Ok(models) = fetch_models_from_upstream(
            &self.client_for_url(&url),
            &upstream.base_url,
            &api_key,
            self.config.admin_upstream_timeout_seconds.max(1),
        )
        .await
        else {
            return;
        };
        let current = self.routing_snapshot().await;
        let Some(current_upstream) = current
            .upstreams
            .iter()
            .find(|candidate| candidate.id == snapshot.upstream_id)
        else {
            return;
        };
        if !snapshot.matches(current_upstream) {
            return;
        }

        let observation_key = MissingObservationKey {
            upstream_id: job.key.upstream_id.clone(),
            key_fingerprint: job.key.key_fingerprint.clone(),
            model: job.target_model.clone(),
        };
        if models.iter().any(|model| model == &job.target_model) {
            self.model_key_sync_runtime
                .lock()
                .expect("model key sync runtime lock poisoned")
                .missing_observations
                .remove(&observation_key);
            for protocol in current_upstream.supported_protocols() {
                self.clear_route_health(&RouteHealthKey {
                    upstream_id: job.key.upstream_id.clone(),
                    key_fingerprint: job.key.key_fingerprint.clone(),
                    runtime_model_slug: job.target_model.clone(),
                    protocol: WireProtocol::from(protocol),
                })
                .await;
            }
            return;
        }

        let now = Instant::now();
        let confirmed = {
            let mut runtime = self
                .model_key_sync_runtime
                .lock()
                .expect("model key sync runtime lock poisoned");
            match runtime.missing_observations.get_mut(&observation_key) {
                Some(observation)
                    if observation.configuration_fingerprint
                        == snapshot.configuration_fingerprint =>
                {
                    if now.duration_since(observation.last_successful_missing_at)
                        >= MISSING_MODEL_CONFIRMATION_INTERVAL
                    {
                        observation.count = observation.count.saturating_add(1);
                        observation.last_successful_missing_at = now;
                    }
                    observation.count >= 2
                }
                _ => {
                    runtime.missing_observations.insert(
                        observation_key.clone(),
                        MissingObservation {
                            count: 1,
                            last_successful_missing_at: now,
                            configuration_fingerprint: snapshot.configuration_fingerprint.clone(),
                        },
                    );
                    false
                }
            }
        };
        if !confirmed || current_upstream.api_key_models.is_empty() {
            return;
        }

        let removed = self
            .mutate_persisted_state_io(|candidate| {
                let Some(upstream) = candidate
                    .upstreams
                    .iter_mut()
                    .find(|upstream| upstream.id == snapshot.upstream_id)
                else {
                    return Ok(false);
                };
                if !snapshot.matches(upstream) {
                    return Ok(false);
                }
                let Some(mapping) = upstream.api_key_models.iter_mut().find(|mapping| {
                    upstream_key_fingerprint(&upstream.id, &mapping.api_key)
                        == job.key.key_fingerprint
                }) else {
                    return Ok(false);
                };
                let previous_len = mapping.supported_models.len();
                mapping
                    .supported_models
                    .retain(|model| model != &job.target_model);
                if mapping.supported_models.len() == previous_len {
                    return Ok(false);
                }
                upstream.supported_models = derive_supported_models(&upstream.api_key_models);
                upstream.last_synced_at = unix_seconds();
                upstream.normalize_for_storage();
                Ok(true)
            })
            .await
            .unwrap_or(false);
        if removed {
            self.model_key_sync_runtime
                .lock()
                .expect("model key sync runtime lock poisoned")
                .missing_observations
                .remove(&observation_key);
            let current_upstreams = self.snapshot().await.upstreams;
            self.reconcile_route_health(&current_upstreams).await;
        }
    }

    pub async fn sync_upstream_model_key_mappings(&self) -> io::Result<ModelKeySyncSummary> {
        self.sync_upstream_model_key_mappings_inner(false).await
    }

    async fn sync_upstream_model_key_mappings_inner(
        &self,
        jitter_upstreams: bool,
    ) -> io::Result<ModelKeySyncSummary> {
        let _sync_guard = self.model_key_sync_lock.lock().await;
        let routing = self.routing_snapshot().await;
        let timeout_seconds = self.config.admin_upstream_timeout_seconds.max(1);
        let mut summary = ModelKeySyncSummary::default();
        let mut discoveries = Vec::new();

        for upstream in routing.upstreams.iter().filter(|upstream| upstream.active) {
            let snapshot = ModelKeySyncSnapshot::capture(upstream);
            let keys = snapshot.raw_keys();
            if keys.is_empty() {
                continue;
            }
            if jitter_upstreams {
                tokio::time::sleep(ModelKeySyncService::upstream_delay(upstream)).await;
            }
            summary.upstreams_scanned = summary.upstreams_scanned.saturating_add(1);
            let url = crate::util::join_upstream_url(&upstream.base_url, "/v1/models");
            let results = fetch_models_from_upstream_keys_concurrently(
                &self.client_for_url(&url),
                &upstream.base_url,
                &keys,
                timeout_seconds,
            )
            .await;
            summary.keys_succeeded = summary.keys_succeeded.saturating_add(
                results
                    .iter()
                    .filter(|result| result.error.is_none())
                    .count(),
            );
            summary.keys_failed = summary.keys_failed.saturating_add(
                results
                    .iter()
                    .filter(|result| result.error.is_some())
                    .count(),
            );
            discoveries.push(CompletedDiscovery { snapshot, results });
        }

        if summary.keys_succeeded == 0 {
            summary.upstreams_unchanged = summary.upstreams_scanned;
            return Ok(summary);
        }

        let synced_at = unix_seconds();
        let now = Instant::now();
        let mut missing_observations = self
            .model_key_sync_runtime
            .lock()
            .expect("model key sync runtime lock poisoned")
            .missing_observations
            .clone();
        let applied = self
            .mutate_persisted_state_io(|candidate| {
                let mut applied = Vec::with_capacity(discoveries.len());
                for discovery in &discoveries {
                    let Some(upstream) = candidate
                        .upstreams
                        .iter_mut()
                        .find(|upstream| upstream.id == discovery.snapshot.upstream_id)
                    else {
                        applied.push(None);
                        continue;
                    };
                    if !upstream.active || !discovery.snapshot.matches(upstream) {
                        applied.push(None);
                        continue;
                    }
                    applied.push(Some(apply_discovery(
                        upstream,
                        &discovery.snapshot,
                        &discovery.results,
                        synced_at,
                        now,
                        &mut missing_observations,
                    )));
                }
                Ok(applied)
            })
            .await?;
        self.model_key_sync_runtime
            .lock()
            .expect("model key sync runtime lock poisoned")
            .missing_observations = missing_observations;

        for result in applied {
            match result {
                Some(true) => {
                    summary.upstreams_updated = summary.upstreams_updated.saturating_add(1)
                }
                Some(false) => {
                    summary.upstreams_unchanged = summary.upstreams_unchanged.saturating_add(1)
                }
                None => summary.skipped = summary.skipped.saturating_add(1),
            }
        }
        let current_upstreams = self.snapshot().await.upstreams;
        self.reconcile_route_health(&current_upstreams).await;
        Ok(summary)
    }
}

impl ModelKeySyncService {
    pub fn startup_delay(state: &AppState) -> Duration {
        deterministic_jitter(&state.store_path.to_string_lossy(), 30, 61)
    }

    pub fn upstream_delay(upstream: &UpstreamConfig) -> Duration {
        let snapshot = ModelKeySyncSnapshot::capture(upstream);
        deterministic_jitter(
            &format!(
                "{}:{}",
                snapshot.upstream_id, snapshot.configuration_fingerprint
            ),
            1,
            30,
        )
    }

    pub fn spawn(state: AppState) -> Option<JoinHandle<()>> {
        let interval_seconds = state.config.upstream_model_key_sync_interval_seconds;
        if interval_seconds == 0 {
            return None;
        }
        let (sender, receiver) = mpsc::channel(TARGETED_DISCOVERY_QUEUE_CAPACITY);
        let mut runtime = state
            .model_key_sync_runtime
            .lock()
            .expect("model key sync runtime lock poisoned");
        if runtime.targeted_sender.is_some() {
            return None;
        }
        runtime.targeted_sender = Some(sender);
        drop(runtime);
        Some(tokio::spawn(run_model_key_sync_service(
            state,
            receiver,
            Duration::from_secs(interval_seconds),
        )))
    }
}

async fn run_model_key_sync_service(
    state: AppState,
    mut targeted: mpsc::Receiver<TargetedModelDiscoveryJob>,
    interval: Duration,
) {
    let _service_guard = ModelKeySyncServiceGuard {
        state: state.clone(),
    };
    let startup_delay = ModelKeySyncService::startup_delay(&state);
    let startup = tokio::time::sleep(startup_delay);
    tokio::pin!(startup);
    loop {
        tokio::select! {
            job = targeted.recv() => match job {
                Some(job) => state.process_targeted_model_discovery(job).await,
                None => return,
            },
            _ = &mut startup => break,
        }
    }

    loop {
        {
            let mut runtime = state
                .model_key_sync_runtime
                .lock()
                .expect("model key sync runtime lock poisoned");
            runtime.periodic_cycles_started = runtime.periodic_cycles_started.saturating_add(1);
        }
        match state.sync_upstream_model_key_mappings_inner(true).await {
            Ok(summary) => tracing::info!(
                scanned = summary.upstreams_scanned,
                updated = summary.upstreams_updated,
                unchanged = summary.upstreams_unchanged,
                skipped = summary.skipped,
                keys_succeeded = summary.keys_succeeded,
                keys_failed = summary.keys_failed,
                "model-key sync cycle completed"
            ),
            Err(error) => tracing::warn!(error = %error, "model-key sync cycle failed"),
        }
        let deadline = Instant::now() + interval;
        loop {
            tokio::select! {
                job = targeted.recv() => match job {
                    Some(job) => state.process_targeted_model_discovery(job).await,
                    None => return,
                },
                _ = tokio::time::sleep_until(deadline) => break,
            }
        }
    }
}
