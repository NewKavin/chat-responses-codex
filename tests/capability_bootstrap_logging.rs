use chat_responses_codex::state::{AppConfig, AppState};
use std::io::Write;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;
use tracing::instrument::WithSubscriber;

#[derive(Clone, Default)]
struct CapabilityTracingCapture {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl CapabilityTracingCapture {
    fn contents(&self) -> String {
        String::from_utf8_lossy(&self.bytes.lock().unwrap()).into_owned()
    }
}

struct CapabilityTracingWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl Write for CapabilityTracingWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.bytes.lock().unwrap().extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for CapabilityTracingCapture {
    type Writer = CapabilityTracingWriter;

    fn make_writer(&'writer self) -> Self::Writer {
        CapabilityTracingWriter {
            bytes: self.bytes.clone(),
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn capability_bootstrap_logs_only_bounded_initialization_metadata() {
    let capture = CapabilityTracingCapture::default();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_target(false)
        .with_writer(capture.clone())
        .finish();
    let dispatch = tracing::Dispatch::new(subscriber);
    let directory = tempdir().unwrap();

    let state = AppState::load_from_path(directory.path().join("state.json"), AppConfig::default())
        .with_subscriber(dispatch)
        .await
        .unwrap();
    let snapshot = state.capability_snapshot();
    let contents = capture.contents();
    let line = contents
        .lines()
        .find(|line| line.contains("initialized capability policy"))
        .unwrap_or_else(|| {
            panic!("startup should log bounded capability policy metadata; captured: {contents:?}")
        })
        .to_string();

    assert!(line.contains("capability_policy_bootstrapped=true"));
    assert!(line.contains(&format!(
        "capability_policy_revision={}",
        snapshot.configuration.source().revision
    )));
    assert!(line.contains(&format!(
        "capability_policy_count={}",
        snapshot.configuration.source().policies.len()
    )));
    for forbidden in [
        "agent_core",
        "domestic-deepseek-family",
        "https://",
        "upstream_id",
        "key_fingerprint",
        "configuration_digest",
    ] {
        assert!(
            !line.contains(forbidden),
            "startup log leaked {forbidden}: {line}"
        );
    }
}
