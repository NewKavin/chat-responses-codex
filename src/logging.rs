use std::fs;
use std::io;
use std::path::Path;
use tracing_appender::rolling::{RollingFileAppender, Rotation};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogRotationCadence {
    Never,
    Hourly,
    Daily,
}

pub fn log_rotation_cadence_from_env(
    read: impl FnOnce() -> Option<String>,
) -> LogRotationCadence {
    match read()
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("never" | "off" | "false") => LogRotationCadence::Never,
        Some("hourly") => LogRotationCadence::Hourly,
        _ => LogRotationCadence::Daily,
    }
}

fn rotation_from_cadence(cadence: LogRotationCadence) -> Rotation {
    match cadence {
        LogRotationCadence::Never => Rotation::NEVER,
        LogRotationCadence::Hourly => Rotation::HOURLY,
        LogRotationCadence::Daily => Rotation::DAILY,
    }
}

pub fn prepare_rolling_log_appender(
    directory: &Path,
    file_prefix: &str,
    cadence: LogRotationCadence,
    max_files: Option<usize>,
) -> io::Result<RollingFileAppender> {
    fs::create_dir_all(directory)?;
    if let Some(max_files) = max_files {
        let mut names: Vec<std::path::PathBuf> = fs::read_dir(directory)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(&format!("{file_prefix}.")))
            })
            .collect();
        names.sort();

        // Keep the newest (max_files - 1) archives; the new current file fills
        // the remaining slot.
        let excess = names.len().saturating_sub(max_files.saturating_sub(1));
        for path in names.into_iter().take(excess) {
            let _ = fs::remove_file(path);
        }
    }

    let mut builder = RollingFileAppender::builder()
        .rotation(rotation_from_cadence(cadence))
        .filename_prefix(file_prefix);
    if let Some(max_files) = max_files {
        builder = builder.max_log_files(max_files);
    }
    builder
        .build(directory)
        .map_err(|error| io::Error::other(error.to_string()))
}
