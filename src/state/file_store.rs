use super::{DownstreamUsageSummary, PersistedState, UsageLog, UsageLogPage, UsageLogQuery};
use crate::capabilities::{
    CapabilityConfiguration, CapabilityStateDocument, DialectProfileKey, UpstreamDialectProfile,
};
use crate::state::{StateStore, StoreFuture};
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs;
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Clone)]
pub struct FileStateStore {
    config_path: PathBuf,
    capability_write_lock: Arc<Mutex<()>>,
}

impl FileStateStore {
    pub fn new(config_path: PathBuf) -> Self {
        Self {
            config_path,
            capability_write_lock: Arc::new(Mutex::new(())),
        }
    }

    fn usage_batch_path(&self) -> PathBuf {
        let base_name = self
            .config_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("state.json");
        let batch_name = format!(
            "{base_name}.usage.{:020}-{}.json",
            unix_millis(),
            Uuid::new_v4()
        );
        self.config_path.with_file_name(batch_name)
    }

    fn capability_path(&self) -> PathBuf {
        let name = self
            .config_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("state.json");
        self.config_path
            .with_file_name(format!("{name}.capabilities.json"))
    }

    async fn load_capability_document(&self) -> io::Result<CapabilityStateDocument> {
        let path = self.capability_path();
        if !fs::try_exists(&path).await? {
            return Ok(CapabilityStateDocument::default());
        }
        serde_json::from_slice(&fs::read(path).await?)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    async fn write_capability_document(
        &self,
        document: &CapabilityStateDocument,
    ) -> io::Result<()> {
        let path = self.capability_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let bytes = serde_json::to_vec_pretty(document)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let tmp_path = path.with_extension("json.tmp");
        fs::write(&tmp_path, bytes).await?;
        fs::rename(tmp_path, path).await
    }
}

impl StateStore for FileStateStore {
    fn persist_config<'a>(&'a self, state: &'a PersistedState) -> StoreFuture<'a, io::Result<()>> {
        Box::pin(async move {
            if let Some(parent) = self.config_path.parent() {
                fs::create_dir_all(parent).await?;
            }

            let bytes = serde_json::to_vec_pretty(&PersistedState {
                upstreams: state.upstreams.clone(),
                downstreams: state.downstreams.clone(),
                usage_logs: Vec::new(),
                announcement: state.announcement.clone(),
                global_context_profiles: state.global_context_profiles.clone(),
            })
            .map_err(io::Error::other)?;

            let tmp_path = self.config_path.with_extension("tmp");
            fs::write(&tmp_path, &bytes).await?;
            fs::rename(&tmp_path, &self.config_path).await
        })
    }

    fn load_capability_state<'a>(&'a self) -> StoreFuture<'a, io::Result<CapabilityStateDocument>> {
        Box::pin(async move { self.load_capability_document().await })
    }

    fn persist_capability_configuration<'a>(
        &'a self,
        config: &'a CapabilityConfiguration,
    ) -> StoreFuture<'a, io::Result<()>> {
        Box::pin(async move {
            let _guard = self.capability_write_lock.lock().await;
            let mut document = self.load_capability_document().await?;
            document.configuration = config.clone();
            self.write_capability_document(&document).await
        })
    }

    fn upsert_dialect_profile<'a>(
        &'a self,
        profile: &'a UpstreamDialectProfile,
    ) -> StoreFuture<'a, io::Result<()>> {
        Box::pin(async move {
            let _guard = self.capability_write_lock.lock().await;
            let mut document = self.load_capability_document().await?;
            document
                .profiles
                .insert(profile.key.clone(), profile.clone());
            self.write_capability_document(&document).await
        })
    }

    fn delete_dialect_profiles_for_upstream<'a>(
        &'a self,
        upstream_id: &'a str,
    ) -> StoreFuture<'a, io::Result<()>> {
        Box::pin(async move {
            let _guard = self.capability_write_lock.lock().await;
            let mut document = self.load_capability_document().await?;
            document
                .profiles
                .retain(|key, _| key.upstream_id != upstream_id);
            self.write_capability_document(&document).await
        })
    }

    fn delete_dialect_profile<'a>(
        &'a self,
        key: &'a DialectProfileKey,
    ) -> StoreFuture<'a, io::Result<()>> {
        Box::pin(async move {
            let _guard = self.capability_write_lock.lock().await;
            let mut document = self.load_capability_document().await?;
            document.profiles.remove(key);
            self.write_capability_document(&document).await
        })
    }

    fn append_usage_logs<'a>(&'a self, logs: &'a [UsageLog]) -> StoreFuture<'a, io::Result<()>> {
        Box::pin(async move {
            if logs.is_empty() {
                return Ok(());
            }
            if let Some(parent) = self.config_path.parent() {
                fs::create_dir_all(parent).await?;
            }

            let batch_path = self.usage_batch_path();
            let bytes = serde_json::to_vec(logs).map_err(io::Error::other)?;
            let tmp_path = batch_path.with_extension("tmp");
            fs::write(&tmp_path, &bytes).await?;
            fs::rename(&tmp_path, &batch_path).await
        })
    }

    fn query_usage_logs_page<'a>(
        &'a self,
        _query: &'a UsageLogQuery,
    ) -> StoreFuture<'a, io::Result<Option<UsageLogPage>>> {
        Box::pin(async { Ok(None) })
    }

    fn downstream_usage_summary<'a>(
        &'a self,
        _downstream_id: &'a str,
    ) -> StoreFuture<'a, io::Result<Option<DownstreamUsageSummary>>> {
        Box::pin(async { Ok(None) })
    }
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}
