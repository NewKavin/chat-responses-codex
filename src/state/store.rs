use super::{DownstreamUsageSummary, PersistedState, UsageLog, UsageLogPage, UsageLogQuery};
use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use tokio::fs;

pub type StoreFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait StateStore: Send + Sync {
    fn persist_config<'a>(&'a self, state: &'a PersistedState) -> StoreFuture<'a, io::Result<()>>;

    fn append_usage_logs<'a>(&'a self, _logs: &'a [UsageLog]) -> StoreFuture<'a, io::Result<()>> {
        Box::pin(async { Ok(()) })
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

#[derive(Clone)]
pub struct FileStateStore {
    store_path: PathBuf,
}

impl FileStateStore {
    pub fn new(store_path: PathBuf) -> Self {
        Self { store_path }
    }
}

impl StateStore for FileStateStore {
    fn persist_config<'a>(&'a self, state: &'a PersistedState) -> StoreFuture<'a, io::Result<()>> {
        Box::pin(async move {
            if let Some(parent) = self.store_path.parent() {
                fs::create_dir_all(parent).await?;
            }

            let bytes = serde_json::to_vec_pretty(state)
                .map_err(|error| io::Error::new(io::ErrorKind::Other, error))?;
            let tmp_path = self.store_path.with_extension("tmp");
            fs::write(&tmp_path, &bytes).await?;
            fs::rename(&tmp_path, &self.store_path).await
        })
    }
}
