use super::{
    CalendarRange, DailyStats, DownstreamUsageSummary, PersistedState, UsageLog, UsageLogPage,
    UsageLogQuery,
};
use crate::capabilities::{
    CapabilityConfiguration, CapabilityStateDocument, DialectProfileKey, UpstreamDialectProfile,
};
use std::future::Future;
use std::io;
use std::pin::Pin;

pub type StoreFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait StateStore: Send + Sync {
    /// Stable backend identifier surfaced in admin error details so
    /// operators can tell whether a persistence failure came from the file
    /// backend or the PostgreSQL backend.
    fn backend_name(&self) -> &'static str {
        "file"
    }

    fn persist_config<'a>(&'a self, state: &'a PersistedState) -> StoreFuture<'a, io::Result<()>>;

    fn load_capability_state<'a>(&'a self) -> StoreFuture<'a, io::Result<CapabilityStateDocument>> {
        Box::pin(async { Ok(CapabilityStateDocument::default()) })
    }

    fn persist_capability_configuration<'a>(
        &'a self,
        _config: &'a CapabilityConfiguration,
    ) -> StoreFuture<'a, io::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn upsert_dialect_profile<'a>(
        &'a self,
        _profile: &'a UpstreamDialectProfile,
    ) -> StoreFuture<'a, io::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn delete_dialect_profiles_for_upstream<'a>(
        &'a self,
        _upstream_id: &'a str,
    ) -> StoreFuture<'a, io::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn delete_dialect_profile<'a>(
        &'a self,
        _key: &'a DialectProfileKey,
    ) -> StoreFuture<'a, io::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn append_usage_logs<'a>(&'a self, _logs: &'a [UsageLog]) -> StoreFuture<'a, io::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn query_usage_logs_page<'a>(
        &'a self,
        _query: &'a UsageLogQuery,
    ) -> StoreFuture<'a, io::Result<Option<UsageLogPage>>> {
        Box::pin(async { Ok(None) })
    }

    fn query_usage_logs_window<'a>(
        &'a self,
        _start_time: u64,
        _end_time: u64,
    ) -> StoreFuture<'a, io::Result<Option<Vec<UsageLog>>>> {
        Box::pin(async { Ok(None) })
    }

    fn downstream_usage_summary<'a>(
        &'a self,
        _downstream_id: &'a str,
    ) -> StoreFuture<'a, io::Result<Option<DownstreamUsageSummary>>> {
        Box::pin(async { Ok(None) })
    }

    fn downstream_daily_stats<'a>(
        &'a self,
        _downstream_id: &'a str,
        _calendar: &'a CalendarRange,
    ) -> StoreFuture<'a, io::Result<Option<Vec<DailyStats>>>> {
        Box::pin(async { Ok(None) })
    }

    fn delete_usage_logs_before<'a>(&'a self, _cutoff: u64) -> StoreFuture<'a, io::Result<()>> {
        Box::pin(async { Ok(()) })
    }
}
