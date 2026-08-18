use chat_responses_codex::logging::{
    log_rotation_cadence_from_env, prepare_rolling_log_appender, LogRotationCadence,
};
use std::fs;
use std::io::Write;

#[test]
fn log_rotation_policy_defaults_to_daily_and_honors_configuration() {
    assert_eq!(
        log_rotation_cadence_from_env(|| None),
        LogRotationCadence::Daily
    );
    assert_eq!(
        log_rotation_cadence_from_env(|| Some("never".into())),
        LogRotationCadence::Never
    );
    assert_eq!(
        log_rotation_cadence_from_env(|| Some("hourly".into())),
        LogRotationCadence::Hourly
    );
    assert_eq!(
        log_rotation_cadence_from_env(|| Some("bogus".into())),
        LogRotationCadence::Daily
    );
}

#[test]
fn rolling_log_appender_prunes_files_beyond_max_when_reopened() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("nested").join("logs");
    let prefix = "gateway";

    let mut first =
        prepare_rolling_log_appender(&dir, prefix, LogRotationCadence::Hourly, Some(2)).unwrap();
    first.write_all(b"current line\n").unwrap();

    let current_path = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&format!("{prefix}.")))
        })
        .expect("rolling appender must create the current log file");

    let expired = dir.join(format!("{prefix}.1999010100"));
    fs::write(&expired, "expired").unwrap();

    let _second =
        prepare_rolling_log_appender(&dir, prefix, LogRotationCadence::Hourly, Some(2)).unwrap();

    assert!(current_path.exists(), "current log must be retained");
    assert!(!expired.exists(), "expired log beyond max_files must be pruned");
}
