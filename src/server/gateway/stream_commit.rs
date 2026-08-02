//! Shared first-semantic-output deadline and stream commit tracking.
//!
//! When multiple routing attempts, protocol translations, or hedging paths
//! participate in a single downstream stream, they must all share one deadline
//! for first semantic output.  After semantic output is observed the request
//! is committed: replay (route-switch) is forbidden and exactly one typed
//! failure is emitted if the stream breaks.

#![allow(dead_code)]

use std::sync::Arc;

use serde_json::Value;
use tokio::time::Instant;

use super::errors::GatewayError;
use super::EndpointKind;

// ── StreamCommitTracker ────────────────────────────────────────────

#[derive(Clone, Debug, Default)]
pub(super) struct StreamCommitTracker {
    inner: Arc<std::sync::Mutex<StreamCommitState>>,
}

#[derive(Debug, Default)]
struct StreamCommitState {
    transport_committed: bool,
    semantic_output_observed: bool,
    terminal_observed: bool,
    last_semantic_at: Option<Instant>,
    last_keepalive_at: Option<Instant>,
}

impl StreamCommitTracker {
    pub(super) fn commit_transport(&self) {
        let mut guard = self.inner.lock().expect("poisoned");
        guard.transport_committed = true;
    }

    pub(super) fn observe_keepalive(&self, now: Instant) {
        let mut guard = self.inner.lock().expect("poisoned");
        guard.last_keepalive_at = Some(now);
    }

    /// Inspect a parsed JSON event and update semantic / terminal flags.
    ///
    /// Chat Completions semantic triggers:
    ///   - `choices[].delta.content` (non-empty string)
    ///   - `choices[].delta.reasoning_content` (non-empty string)
    ///   - `choices[].delta.tool_calls[].id` (non-empty)
    ///   - `choices[].delta.tool_calls[].function.name` (non-empty)
    ///   - `choices[].delta.tool_calls[].function.arguments` (non-empty,
    ///     including partial JSON like `{`)
    ///   - `choices[].finish_reason` non-null → terminal
    ///
    /// Responses semantic triggers:
    ///   - `response.output_text.delta` with non-empty `delta`
    ///   - `response.reasoning_summary_text.delta` with non-empty `delta`
    ///   - `response.output_item.added` with `function_call` item + non-empty
    ///     `call_id`
    ///   - `response.function_call_arguments.delta` with non-empty `delta`
    ///   - `response.completed` → terminal
    ///
    /// Role-only deltas, empty strings, `response.created`,
    /// `response.in_progress`, comments, and keepalives do **not** count.
    pub(super) fn observe_json(&self, endpoint: EndpointKind, event: &Value) {
        let mut guard = self.inner.lock().expect("poisoned");
        let now = Instant::now();

        match endpoint {
            EndpointKind::ChatCompletions => {
                if let Some(choices) = event.get("choices").and_then(Value::as_array) {
                    for choice in choices {
                        if let Some(delta) = choice.get("delta") {
                            if has_non_empty_string(delta, "content")
                                || has_non_empty_string(delta, "reasoning_content")
                            {
                                guard.semantic_output_observed = true;
                                guard.last_semantic_at = Some(now);
                            }
                            if let Some(tool_calls) =
                                delta.get("tool_calls").and_then(Value::as_array)
                            {
                                for tc in tool_calls {
                                    if has_non_empty_string(tc, "id")
                                        || has_non_empty_nested_string(tc, &["function", "name"])
                                        || has_non_empty_nested_string(
                                            tc,
                                            &["function", "arguments"],
                                        )
                                    {
                                        guard.semantic_output_observed = true;
                                        guard.last_semantic_at = Some(now);
                                    }
                                }
                            }
                        }
                        // finish_reason present and non-null → terminal
                        if choice.get("finish_reason").is_some()
                            && !choice.get("finish_reason").is_none_or(|v| v.is_null())
                        {
                            guard.terminal_observed = true;
                        }
                    }
                }
            }
            EndpointKind::Responses => {
                let event_type = event.get("type").and_then(Value::as_str).unwrap_or("");
                match event_type {
                    "response.output_text.delta" | "response.reasoning_summary_text.delta"
                        if has_non_empty_string(event, "delta") =>
                    {
                        guard.semantic_output_observed = true;
                        guard.last_semantic_at = Some(now);
                    }
                    "response.output_item.added"
                        if let Some(item) = event.get("item")
                            && item.get("type").and_then(Value::as_str)
                                == Some("function_call")
                            && has_non_empty_string(item, "call_id") =>
                    {
                        guard.semantic_output_observed = true;
                        guard.last_semantic_at = Some(now);
                    }
                    "response.function_call_arguments.delta"
                        if has_non_empty_string(event, "delta") =>
                    {
                        guard.semantic_output_observed = true;
                        guard.last_semantic_at = Some(now);
                    }
                    "response.completed" => {
                        guard.terminal_observed = true;
                    }
                    _ => {}
                }
            }
        }
    }

    pub(super) fn transport_committed(&self) -> bool {
        self.inner.lock().expect("poisoned").transport_committed
    }

    pub(super) fn semantic_output_observed(&self) -> bool {
        self.inner
            .lock()
            .expect("poisoned")
            .semantic_output_observed
    }

    pub(super) fn terminal_observed(&self) -> bool {
        self.inner.lock().expect("poisoned").terminal_observed
    }

    /// Replay (route-switch) is only allowed before any semantic output.
    pub(super) fn can_replay(&self) -> bool {
        !self.semantic_output_observed()
    }

    pub(super) fn last_semantic_at(&self) -> Option<Instant> {
        self.inner.lock().expect("poisoned").last_semantic_at
    }

    pub(super) fn last_keepalive_at(&self) -> Option<Instant> {
        self.inner.lock().expect("poisoned").last_keepalive_at
    }
}

fn has_non_empty_string(value: &Value, field: &str) -> bool {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(|s| !s.is_empty())
        .unwrap_or(false)
}

fn has_non_empty_nested_string(value: &Value, path: &[&str]) -> bool {
    let mut current = value;
    for &segment in path {
        match current.get(segment) {
            Some(next) => current = next,
            None => return false,
        }
    }
    current.as_str().map(|s| !s.is_empty()).unwrap_or(false)
}

// ── FirstSemanticDeadline ─────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub(super) struct FirstSemanticDeadline {
    started: Instant,
    deadline: Instant,
}

impl FirstSemanticDeadline {
    pub(super) fn new(started: Instant, budget: std::time::Duration) -> Self {
        Self {
            started,
            deadline: started + budget,
        }
    }

    /// Remaining budget.  Returns an error if the deadline has passed.
    pub(super) fn remaining(self) -> Result<std::time::Duration, GatewayError> {
        let now = Instant::now();
        if now >= self.deadline {
            return Err(first_semantic_output_timeout_error());
        }
        Ok(self.deadline - now)
    }

    /// Clip a phase timeout to the remaining budget.
    /// Returns the smaller of `remaining()` and `phase_limit`.
    /// Errors if the deadline has already passed.
    pub(super) fn clip(
        self,
        phase_limit: std::time::Duration,
    ) -> Result<std::time::Duration, GatewayError> {
        let remaining = self.remaining()?;
        Ok(remaining.min(phase_limit))
    }

    pub(super) fn started(self) -> Instant {
        self.started
    }

    /// The absolute deadline instant.  Used to race against the deadline
    /// in a `tokio::select!` alongside upstream stream reads.
    pub(super) fn deadline(self) -> Instant {
        self.deadline
    }
}

/// The canonical error emitted when the shared first-semantic-output
/// deadline expires.  This is a logical 504 that must not produce a
/// 499 or cause payload replay.
pub(super) fn first_semantic_output_timeout_error() -> GatewayError {
    GatewayError::GatewayTimeout(
        "first_semantic_output_timeout: no semantic output within the shared budget".into(),
    )
}

// ── Unit tests ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_non_empty_output_field_blocks_replay() {
        let cases = [
            (
                EndpointKind::ChatCompletions,
                json!({"choices":[{"delta":{"content":"x"}}]}),
            ),
            (
                EndpointKind::ChatCompletions,
                json!({"choices":[{"delta":{"reasoning_content":"r"}}]}),
            ),
            (
                EndpointKind::ChatCompletions,
                json!({"choices":[{"delta":{"tool_calls":[{"id":"call_1"}]}}]}),
            ),
            (
                EndpointKind::ChatCompletions,
                json!({"choices":[{"delta":{"tool_calls":[{"function":{"name":"read_file"}}]}}]}),
            ),
            (
                EndpointKind::ChatCompletions,
                json!({"choices":[{"delta":{"tool_calls":[{"function":{"arguments":"{"}}]}}]}),
            ),
            (
                EndpointKind::Responses,
                json!({"type":"response.output_text.delta","delta":"x"}),
            ),
            (
                EndpointKind::Responses,
                json!({"type":"response.reasoning_summary_text.delta","delta":"r"}),
            ),
            (
                EndpointKind::Responses,
                json!({"type":"response.output_item.added","item":{"type":"function_call","call_id":"call_1"}}),
            ),
            (
                EndpointKind::Responses,
                json!({"type":"response.function_call_arguments.delta","delta":"{"}),
            ),
        ];
        for (endpoint, event) in cases {
            let tracker = StreamCommitTracker::default();
            tracker.observe_json(endpoint, &event);
            assert!(
                tracker.semantic_output_observed(),
                "expected semantic output for {event}"
            );
            assert!(!tracker.can_replay(), "expected replay blocked for {event}");
        }
    }

    #[test]
    fn empty_and_non_semantic_events_do_not_block_replay() {
        let cases = [
            (
                EndpointKind::ChatCompletions,
                json!({"choices":[{"delta":{"content":""}}]}),
            ),
            (
                EndpointKind::ChatCompletions,
                json!({"choices":[{"delta":{"role":"assistant"}}]}),
            ),
            (
                EndpointKind::ChatCompletions,
                json!({"choices":[{"delta":{}}]}),
            ),
            (EndpointKind::Responses, json!({"type":"response.created"})),
            (
                EndpointKind::Responses,
                json!({"type":"response.in_progress"}),
            ),
            (
                EndpointKind::Responses,
                json!({"type":"response.output_text.delta","delta":""}),
            ),
            (
                EndpointKind::Responses,
                json!({"type":"response.output_item.added","item":{"type":"function_call","call_id":""}}),
            ),
        ];
        for (endpoint, event) in cases {
            let tracker = StreamCommitTracker::default();
            tracker.observe_json(endpoint, &event);
            assert!(
                !tracker.semantic_output_observed(),
                "expected no semantic output for {event}"
            );
            assert!(tracker.can_replay(), "expected replay allowed for {event}");
        }
    }

    #[test]
    fn chat_finish_reason_marks_terminal() {
        let tracker = StreamCommitTracker::default();
        tracker.observe_json(
            EndpointKind::ChatCompletions,
            &json!({"choices":[{"delta":{"content":"hi"},"finish_reason":"stop"}]}),
        );
        assert!(tracker.semantic_output_observed());
        assert!(tracker.terminal_observed());
        assert!(!tracker.can_replay());
    }

    #[test]
    fn responses_completed_marks_terminal() {
        let tracker = StreamCommitTracker::default();
        tracker.observe_json(
            EndpointKind::Responses,
            &json!({"type":"response.completed"}),
        );
        assert!(tracker.terminal_observed());
    }

    #[test]
    fn commit_transport_does_not_block_replay() {
        let tracker = StreamCommitTracker::default();
        tracker.commit_transport();
        assert!(tracker.transport_committed());
        assert!(tracker.can_replay());
        assert!(!tracker.semantic_output_observed());
    }

    #[test]
    fn first_semantic_deadline_clips_to_minimum() {
        let started = Instant::now();
        let deadline = FirstSemanticDeadline::new(started, std::time::Duration::from_secs(600));
        let clipped = deadline.clip(std::time::Duration::from_secs(30)).unwrap();
        assert!(clipped <= std::time::Duration::from_secs(30));
    }

    #[test]
    fn first_semantic_deadline_errors_when_expired() {
        let started = Instant::now();
        let deadline = FirstSemanticDeadline::new(started, std::time::Duration::from_nanos(1));
        // The deadline should have already passed (1 nanosecond).
        let result = deadline.remaining();
        assert!(result.is_err());
    }
}
