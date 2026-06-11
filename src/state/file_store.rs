use super::{DownstreamUsageSummary, PersistedState, UsageLog, UsageLogPage, UsageLogQuery};
use crate::state::{StateStore, StoreFuture};
use std::io;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs;
use uuid::Uuid;

#[derive(Clone)]
pub struct FileStateStore {
    config_path: PathBuf,
}

impl FileStateStore {
    pub fn new(config_path: PathBuf) -> Self {
        Self { config_path }
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
            })
            .map_err(|error| io::Error::new(io::ErrorKind::Other, error))?;

            let tmp_path = self.config_path.with_extension("tmp");
            fs::write(&tmp_path, &bytes).await?;
            fs::rename(&tmp_path, &self.config_path).await
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
            let bytes = serde_json::to_vec(logs)
                .map_err(|error| io::Error::new(io::ErrorKind::Other, error))?;
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
