use crate::keys::verify_downstream_key;
use crate::routing::{
    select_upstream, RouteError, RouteRequest, UpstreamCandidate, UpstreamProtocol,
};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs;
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub admin_username: String,
    pub admin_password: String,
    pub app_name: String,
    pub usage_log_rotation_max_bytes: usize,
    pub usage_log_archive_max_files: usize,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            admin_username: "admin".into(),
            admin_password: "admin".into(),
            app_name: "chat2responses-gateway".into(),
            usage_log_rotation_max_bytes: 1_048_576,
            usage_log_archive_max_files: 10,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamConfig {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub api_key: String,
    pub protocol: UpstreamProtocol,
    pub supported_models: Vec<String>,
    pub active: bool,
    pub failure_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownstreamConfig {
    pub id: String,
    pub name: String,
    pub hash: String,
    pub model_allowlist: Vec<String>,
    pub per_minute_limit: u32,
    pub daily_token_limit: Option<u64>,
    pub monthly_token_limit: Option<u64>,
    pub ip_allowlist: Vec<String>,
    pub expires_at: Option<u64>,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageLog {
    pub id: String,
    pub downstream_key_id: String,
    pub upstream_key_id: String,
    pub endpoint: String,
    pub model: String,
    pub request_id: String,
    pub status_code: u16,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub latency_ms: u64,
    pub created_at: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PersistedState {
    pub upstreams: Vec<UpstreamConfig>,
    pub downstreams: Vec<DownstreamConfig>,
    pub usage_logs: Vec<UsageLog>,
}

#[derive(Clone)]
pub struct AppState {
    inner: Arc<Mutex<PersistedState>>,
    archived_usage_logs: Arc<Mutex<Vec<UsageLog>>>,
    downstream_request_windows: Arc<Mutex<HashMap<String, VecDeque<u64>>>>,
    pub store_path: PathBuf,
    pub config: AppConfig,
    client: Client,
}

impl AppState {
    pub fn new(state: PersistedState, store_path: impl Into<PathBuf>, config: AppConfig) -> Self {
        Self::new_with_archived(state, Vec::new(), store_path, config)
    }

    fn new_with_archived(
        state: PersistedState,
        archived_usage_logs: Vec<UsageLog>,
        store_path: impl Into<PathBuf>,
        config: AppConfig,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(state)),
            archived_usage_logs: Arc::new(Mutex::new(archived_usage_logs)),
            downstream_request_windows: Arc::new(Mutex::new(HashMap::new())),
            store_path: store_path.into(),
            config,
            client: Client::new(),
        }
    }

    pub fn client(&self) -> Client {
        self.client.clone()
    }

    pub async fn snapshot(&self) -> PersistedState {
        let mut state = self.inner.lock().await.clone();
        let archived_usage_logs = self.archived_usage_logs.lock().await.clone();
        if archived_usage_logs.is_empty() {
            return state;
        }

        let mut usage_logs = archived_usage_logs
            .into_iter()
            .chain(state.usage_logs.into_iter())
            .collect::<Vec<_>>();
        usage_logs.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then(left.request_id.cmp(&right.request_id))
                .then(left.id.cmp(&right.id))
        });
        let mut seen = HashSet::new();
        let mut deduped = Vec::with_capacity(usage_logs.len());
        for log in usage_logs {
            if seen.insert(log.id.clone()) {
                deduped.push(log);
            }
        }
        state.usage_logs = deduped;
        state
    }

    pub async fn load_from_path(path: impl AsRef<Path>, config: AppConfig) -> io::Result<Self> {
        let store_path = path.as_ref().to_path_buf();
        let state = if fs::try_exists(&store_path).await? {
            let bytes = fs::read(&store_path).await?;
            serde_json::from_slice(&bytes).unwrap_or_default()
        } else {
            PersistedState::default()
        };

        let archived_usage_logs = load_archived_usage_logs(&store_path).await?;
        let app = Self::new_with_archived(state, archived_usage_logs, store_path, config);
        app.enforce_usage_log_archive_limit().await?;
        Ok(app)
    }

    pub async fn persist(&self) -> io::Result<()> {
        let state = self.snapshot().await;
        self.persist_state(&state).await
    }

    pub async fn downstream_for_secret(&self, secret: &str) -> Option<DownstreamConfig> {
        let state = self.inner.lock().await;
        state
            .downstreams
            .iter()
            .find(|downstream| downstream.active && verify_downstream_key(secret, &downstream.hash))
            .cloned()
    }

    pub async fn choose_upstream(
        &self,
        model: &str,
        protocol: UpstreamProtocol,
    ) -> Result<UpstreamConfig, RouteError> {
        let selected = select_upstream(
            &RouteRequest::new(model, protocol, false),
            &self.upstream_candidates().await,
        )?;

        let state = self.inner.lock().await;
        state
            .upstreams
            .iter()
            .find(|upstream| upstream.id == selected.id)
            .cloned()
            .ok_or(RouteError::NoHealthyUpstream(model.to_string()))
    }

    pub async fn upstream_candidates(&self) -> Vec<UpstreamCandidate> {
        let state = self.inner.lock().await;
        state
            .upstreams
            .iter()
            .filter(|upstream| upstream.active)
            .map(|upstream| {
                UpstreamCandidate::new(
                    upstream.id.clone(),
                    upstream.name.clone(),
                    upstream.protocol,
                )
                .with_models(upstream.supported_models.clone())
                .with_failure_count(upstream.failure_count)
            })
            .collect()
    }

    pub async fn mark_upstream_failure(&self, upstream_id: &str) -> io::Result<()> {
        let mut state = self.inner.lock().await;
        if let Some(upstream) = state
            .upstreams
            .iter_mut()
            .find(|upstream| upstream.id == upstream_id)
        {
            upstream.failure_count = upstream.failure_count.saturating_add(1);
        }
        self.persist_state(&state).await
    }

    pub async fn mark_upstream_success(&self, upstream_id: &str) -> io::Result<()> {
        let mut state = self.inner.lock().await;
        if let Some(upstream) = state
            .upstreams
            .iter_mut()
            .find(|upstream| upstream.id == upstream_id)
        {
            upstream.failure_count = 0;
        }
        self.persist_state(&state).await
    }

    pub async fn append_usage_log(&self, mut log: UsageLog) -> io::Result<()> {
        if log.id.is_empty() {
            log.id = Uuid::new_v4().to_string();
        }
        if log.created_at == 0 {
            log.created_at = unix_seconds();
        }

        let mut state = self.inner.lock().await;
        state.usage_logs.push(log);
        let mut candidate_state = state.clone();
        let archived_logs = trim_usage_logs_for_rotation(
            &mut candidate_state,
            self.config.usage_log_rotation_max_bytes,
        );

        if !archived_logs.is_empty() {
            self.write_usage_log_archive(&archived_logs).await?;
            {
                let mut archived = self.archived_usage_logs.lock().await;
                archived.extend(archived_logs);
            }
        }

        self.persist_state(&candidate_state).await?;
        *state = candidate_state;
        self.enforce_usage_log_archive_limit().await?;
        Ok(())
    }

    pub async fn reserve_downstream_request(
        &self,
        downstream_id: &str,
        per_minute_limit: u32,
    ) -> Result<(), u64> {
        let mut windows = self.downstream_request_windows.lock().await;
        let window = windows
            .entry(downstream_id.to_string())
            .or_insert_with(VecDeque::new);
        let now = unix_seconds();
        let window_start = now.saturating_sub(59);

        while let Some(&timestamp) = window.front() {
            if timestamp < window_start {
                window.pop_front();
            } else {
                break;
            }
        }

        if window.len() >= per_minute_limit as usize {
            let oldest = window.front().copied().unwrap_or(now);
            let retry_after = oldest.saturating_add(60).saturating_sub(now).max(1);
            return Err(retry_after);
        }

        window.push_back(now);
        Ok(())
    }

    pub async fn insert_upstream(&self, upstream: UpstreamConfig) -> io::Result<()> {
        let mut state = self.inner.lock().await;
        state.upstreams.push(upstream);
        self.persist_state(&state).await
    }

    pub async fn insert_downstream(&self, downstream: DownstreamConfig) -> io::Result<()> {
        let mut state = self.inner.lock().await;
        state.downstreams.push(downstream);
        self.persist_state(&state).await
    }

    pub async fn set_downstream_active(
        &self,
        downstream_id: &str,
        active: bool,
    ) -> io::Result<bool> {
        let mut state = self.inner.lock().await;
        let Some(downstream) = state
            .downstreams
            .iter_mut()
            .find(|downstream| downstream.id == downstream_id)
        else {
            return Ok(false);
        };
        downstream.active = active;
        self.persist_state(&state).await?;
        Ok(true)
    }

    pub async fn set_upstream_active(&self, upstream_id: &str, active: bool) -> io::Result<bool> {
        let mut state = self.inner.lock().await;
        let Some(upstream) = state
            .upstreams
            .iter_mut()
            .find(|upstream| upstream.id == upstream_id)
        else {
            return Ok(false);
        };
        upstream.active = active;
        self.persist_state(&state).await?;
        Ok(true)
    }

    pub async fn upstreams(&self) -> Vec<UpstreamConfig> {
        self.snapshot().await.upstreams
    }

    pub async fn downstreams(&self) -> Vec<DownstreamConfig> {
        self.snapshot().await.downstreams
    }

    pub async fn usage_logs(&self) -> Vec<UsageLog> {
        self.snapshot().await.usage_logs
    }

    pub async fn available_models_for_downstream(&self, secret: &str) -> Vec<String> {
        let state = self.inner.lock().await;
        let Some(downstream) = state.downstreams.iter().find(|downstream| {
            downstream.active && verify_downstream_key(secret, &downstream.hash)
        }) else {
            return Vec::new();
        };

        let mut models = Vec::new();
        for upstream in &state.upstreams {
            if upstream.active {
                for model in &upstream.supported_models {
                    if (downstream.model_allowlist.is_empty()
                        || downstream.model_allowlist.contains(model))
                        && !models.contains(model)
                    {
                        models.push(model.clone());
                    }
                }
            }
        }
        models.sort();
        models
    }

    async fn persist_state(&self, state: &PersistedState) -> io::Result<()> {
        if let Some(parent) = self.store_path.parent() {
            fs::create_dir_all(parent).await?;
        }

        let bytes = serde_json::to_vec_pretty(state)
            .map_err(|error| io::Error::new(io::ErrorKind::Other, error))?;
        let tmp_path = self.store_path.with_extension("tmp");
        fs::write(&tmp_path, &bytes).await?;
        fs::rename(&tmp_path, &self.store_path).await
    }

    async fn write_usage_log_archive(&self, logs: &[UsageLog]) -> io::Result<()> {
        if logs.is_empty() {
            return Ok(());
        }

        if let Some(parent) = self.store_path.parent() {
            fs::create_dir_all(parent).await?;
        }

        let archive_path = self.usage_log_archive_path();
        let bytes = serde_json::to_vec(logs)
            .map_err(|error| io::Error::new(io::ErrorKind::Other, error))?;
        let tmp_path = archive_path.with_extension("tmp");
        fs::write(&tmp_path, &bytes).await?;
        fs::rename(&tmp_path, &archive_path).await
    }

    fn usage_log_archive_path(&self) -> PathBuf {
        let base_name = self
            .store_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("state.json");
        let archive_name = format!(
            "{base_name}.usage.{:020}-{}.json",
            unix_millis(),
            Uuid::new_v4()
        );
        self.store_path.with_file_name(archive_name)
    }

    async fn enforce_usage_log_archive_limit(&self) -> io::Result<()> {
        let limit = self.config.usage_log_archive_max_files.max(1);
        let archive_paths = usage_log_archive_paths(&self.store_path).await?;
        if archive_paths.len() <= limit {
            return Ok(());
        }

        let remove_count = archive_paths.len() - limit;
        let mut removed_ids = HashSet::new();

        for path in archive_paths.into_iter().take(remove_count) {
            let logs = load_usage_log_archive(&path).await?;
            removed_ids.extend(logs.into_iter().map(|log| log.id));
            match fs::remove_file(&path).await {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }

        if !removed_ids.is_empty() {
            let mut archived = self.archived_usage_logs.lock().await;
            archived.retain(|log| !removed_ids.contains(&log.id));
        }

        Ok(())
    }
}

fn trim_usage_logs_for_rotation(state: &mut PersistedState, max_bytes: usize) -> Vec<UsageLog> {
    let max_bytes = max_bytes.max(1);
    let mut archived_logs = Vec::new();

    while serialized_state_size(state) > max_bytes && !state.usage_logs.is_empty() {
        archived_logs.push(state.usage_logs.remove(0));
    }

    archived_logs
}

fn serialized_state_size(state: &PersistedState) -> usize {
    serde_json::to_vec_pretty(state)
        .map(|bytes| bytes.len())
        .unwrap_or(usize::MAX)
}

async fn load_archived_usage_logs(store_path: &Path) -> io::Result<Vec<UsageLog>> {
    let archive_paths = usage_log_archive_paths(store_path).await?;

    let mut usage_logs = Vec::new();
    for path in archive_paths {
        let mut archived = load_usage_log_archive(&path).await?;
        usage_logs.append(&mut archived);
    }

    Ok(usage_logs)
}

async fn usage_log_archive_paths(store_path: &Path) -> io::Result<Vec<PathBuf>> {
    let Some(parent) = store_path.parent() else {
        return Ok(Vec::new());
    };
    let Some(base_name) = store_path.file_name().and_then(|value| value.to_str()) else {
        return Ok(Vec::new());
    };

    let archive_prefix = format!("{base_name}.usage.");
    let mut dir = fs::read_dir(parent).await?;
    let mut archive_paths = Vec::new();

    while let Some(entry) = dir.next_entry().await? {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if file_name.starts_with(&archive_prefix) && file_name.ends_with(".json") {
            let sort_key = load_usage_log_archive(&path)
                .await
                .ok()
                .and_then(|logs| logs.first().map(|log| log.created_at))
                .unwrap_or(0);
            archive_paths.push((sort_key, file_name.to_string(), path));
        }
    }

    archive_paths.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
    Ok(archive_paths.into_iter().map(|(_, _, path)| path).collect())
}

async fn load_usage_log_archive(path: &Path) -> io::Result<Vec<UsageLog>> {
    let bytes = fs::read(path).await?;
    Ok(serde_json::from_slice(&bytes).unwrap_or_default())
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

pub fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn new_id(prefix: &str) -> String {
    format!("{}_{}", prefix, Uuid::new_v4())
}

pub fn encode_secret_suffix(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}
