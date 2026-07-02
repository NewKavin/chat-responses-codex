use super::common::*;
use axum::response::IntoResponse;
use futures_util::StreamExt;
use serde_json::json;

async fn wait_for_upstream_in_flight(state: &AppState, upstream_id: &str, expected: u32) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let snapshots = state.upstream_runtime_snapshots().await;
        let in_flight = snapshots
            .get(upstream_id)
            .map(|snapshot| snapshot.in_flight)
            .unwrap_or_default();
        if in_flight == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for upstream {upstream_id} in_flight={expected}, saw {in_flight}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

// ============================================================================
// Batch 1: Local Upstream Concurrency Config Tests
// ============================================================================

#[path = "responses/admin_runtime.rs"]
mod admin_runtime;
#[path = "responses/core.rs"]
mod core;
#[path = "responses/fallback.rs"]
mod fallback;
#[path = "responses/history.rs"]
mod history;
#[path = "responses/stream_lifecycle.rs"]
mod stream_lifecycle;
#[path = "responses/streaming.rs"]
mod streaming;
#[path = "responses/upstream_feedback.rs"]
mod upstream_feedback;
