use super::{
    CalendarRange, DailyStats, DownstreamUsageSummary, PersistedState, UsageLog, UsageLogPage,
    UsageLogQuery,
};
use crate::capabilities::{
    CapabilityConfiguration, CapabilityStateDocument, DialectProfileKey, UpstreamDialectProfile,
};
use crate::state::{StateStore, StoreFuture};
use std::collections::HashMap;
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

    async fn delete_usage_logs_before(&self, cutoff: u64) -> io::Result<()> {
        let Some(parent) = self.config_path.parent() else {
            return Ok(());
        };
        let Some(base_name) = self.config_path.file_name().and_then(|v| v.to_str()) else {
            return Ok(());
        };
        let archive_prefix = format!("{base_name}.usage.");
        let mut dir = fs::read_dir(parent).await?;
        while let Some(entry) = dir.next_entry().await? {
            let path = entry.path();
            let Some(file_name) = path.file_name().and_then(|v| v.to_str()) else {
                continue;
            };
            if !file_name.starts_with(&archive_prefix) || !file_name.ends_with(".json") {
                continue;
            }
            let bytes = fs::read(&path).await?;
            let mut logs: Vec<UsageLog> = serde_json::from_slice(&bytes).unwrap_or_default();
            for log in &mut logs {
                log.normalize_after_load();
            }
            let retained: Vec<&UsageLog> =
                logs.iter().filter(|log| log.created_at >= cutoff).collect();
            if retained.is_empty() {
                match fs::remove_file(&path).await {
                    Ok(_) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            } else if retained.len() < logs.len() {
                let kept: Vec<UsageLog> = retained.into_iter().cloned().collect();
                let bytes = serde_json::to_vec(&kept).map_err(io::Error::other)?;
                let tmp_path = path.with_extension("tmp");
                fs::write(&tmp_path, &bytes).await?;
                fs::rename(&tmp_path, &path).await?;
            }
        }
        Ok(())
    }

    async fn usage_archive_paths(&self) -> io::Result<Vec<PathBuf>> {
        let Some(parent) = self.config_path.parent() else {
            return Ok(Vec::new());
        };
        let Some(base_name) = self
            .config_path
            .file_name()
            .and_then(|value| value.to_str())
        else {
            return Ok(Vec::new());
        };

        let archive_prefix = format!("{base_name}.usage.");
        let mut dir = fs::read_dir(parent).await?;
        let mut paths = Vec::new();
        while let Some(entry) = dir.next_entry().await? {
            let path = entry.path();
            let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            if file_name.starts_with(&archive_prefix) && file_name.ends_with(".json") {
                paths.push(path);
            }
        }
        paths.sort();
        Ok(paths)
    }

    async fn usage_log_sources(&self) -> io::Result<Vec<Vec<UsageLog>>> {
        let mut sources = Vec::new();
        if fs::try_exists(&self.config_path).await? {
            let bytes = fs::read(&self.config_path).await?;
            let state: PersistedState = serde_json::from_slice(&bytes)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            if !state.usage_logs.is_empty() {
                sources.push(state.usage_logs);
            }
        }
        for path in self.usage_archive_paths().await? {
            let bytes = fs::read(path).await?;
            let logs: Vec<UsageLog> = serde_json::from_slice(&bytes)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            sources.push(logs);
        }
        for logs in &mut sources {
            for log in logs {
                log.normalize_after_load();
            }
        }
        Ok(sources)
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
                runtime_settings: state.runtime_settings.clone(),
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

    fn query_usage_logs_window<'a>(
        &'a self,
        start_time: u64,
        end_time: u64,
    ) -> StoreFuture<'a, io::Result<Option<Vec<UsageLog>>>> {
        Box::pin(async move {
            let sources = self.usage_log_sources().await?;
            if sources.is_empty() {
                return Ok(None);
            }

            let mut seen = std::collections::HashSet::new();
            let mut logs = Vec::new();
            for source in sources {
                logs.extend(source.into_iter().filter(|log| {
                    log.created_at >= start_time
                        && log.created_at < end_time
                        && seen.insert(log.id.clone())
                }));
            }
            Ok(Some(logs))
        })
    }

    fn downstream_usage_summary<'a>(
        &'a self,
        _downstream_id: &'a str,
    ) -> StoreFuture<'a, io::Result<Option<DownstreamUsageSummary>>> {
        Box::pin(async { Ok(None) })
    }

    fn downstream_daily_stats<'a>(
        &'a self,
        downstream_id: &'a str,
        calendar: &'a CalendarRange,
    ) -> StoreFuture<'a, io::Result<Option<Vec<DailyStats>>>> {
        Box::pin(async move {
            let sources = self.usage_log_sources().await?;
            if sources.is_empty() {
                return Ok(None);
            }

            let mut aggregated = HashMap::<String, (u32, u64, u32)>::new();
            let mut seen = std::collections::HashSet::new();
            for logs in sources {
                for log in logs {
                    if !seen.insert(log.id.clone()) {
                        continue;
                    }
                    if log.downstream_key_id != downstream_id
                        || log.created_at < calendar.start_time
                        || log.created_at >= calendar.end_time
                    {
                        continue;
                    }
                    let Some(day) = calendar.days.iter().find(|day| {
                        log.created_at >= day.start_time && log.created_at < day.end_time
                    }) else {
                        continue;
                    };
                    let entry = aggregated.entry(day.day.clone()).or_default();
                    entry.0 = entry.0.saturating_add(1);
                    entry.1 = entry.1.saturating_add(log.total_tokens);
                    if log.status_code == 200 {
                        entry.2 = entry.2.saturating_add(1);
                    }
                }
            }

            Ok(Some(
                calendar
                    .days
                    .iter()
                    .map(|day| {
                        let (total_requests, total_tokens, successful_requests) =
                            aggregated.get(&day.day).copied().unwrap_or_default();
                        DailyStats {
                            day: day.day.clone(),
                            start_time: day.start_time,
                            total_requests,
                            total_tokens,
                            success_rate: if total_requests > 0 {
                                successful_requests as f64 / total_requests as f64
                            } else {
                                0.0
                            },
                        }
                    })
                    .collect(),
            ))
        })
    }

    fn delete_usage_logs_before<'a>(&'a self, cutoff: u64) -> StoreFuture<'a, io::Result<()>> {
        Box::pin(async move { self.delete_usage_logs_before(cutoff).await })
    }
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}
