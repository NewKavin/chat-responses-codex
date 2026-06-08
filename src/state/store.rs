use super::{DownstreamUsageSummary, PersistedState, UsageLog, UsageLogPage, UsageLogQuery};
use std::future::Future;
use std::io;
use std::pin::Pin;

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
