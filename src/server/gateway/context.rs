use crate::state::{GlobalContextProfile, UpstreamConfig};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

const CONTEXT_KEEP_RECENT_ITEMS: usize = 8;
const CONTEXT_TOOL_RESULT_TRUNCATE_CHARS: usize = 1200;
const CONTEXT_MESSAGE_TRUNCATE_CHARS: usize = 800;

#[derive(Debug, Default, Clone, Copy)]
pub(super) struct ContextTrimStats {
    pub(super) truncated_blocks: u32,
    pub(super) compacted_entries: u32,
    pub(super) tool_result_blocks: u32,
}

#[derive(Debug, Clone)]
pub(super) struct ContextBudgetReport {
    pub(super) estimated_input_tokens: u64,
    pub(super) estimated_input_tokens_after_trim: u64,
    pub(super) protected_minimum_tokens: u64,
    pub(super) compacted_items: u32,
    pub(super) requested_output_tokens: u64,
    pub(super) allowed_input_tokens: u64,
    pub(super) context_limit: u32,
    pub(super) output_reserve: u32,
    pub(super) max_output_tokens_cap: u32,
    pub(super) max_output_tokens_clamped: bool,
    pub(super) trim_stats: ContextTrimStats,
    pub(super) fallback_model: Option<String>,
}

fn requested_output_tokens_from_payload(payload: &Value) -> u64 {
    payload
        .get("max_output_tokens")
        .and_then(Value::as_u64)
        .or_else(|| payload.get("max_tokens").and_then(Value::as_u64))
        .or_else(|| payload.get("max_completion_tokens").and_then(Value::as_u64))
        .unwrap_or(0)
}

fn estimate_tokens_from_text(text: &str) -> u64 {
    let chars = text.chars().count() as u64;
    if chars == 0 {
        0
    } else {
        chars.div_ceil(4)
    }
}

fn estimate_tokens_from_value(value: &Value) -> u64 {
    match value {
        Value::String(text) => estimate_tokens_from_text(text),
        _ => estimate_tokens_from_text(&serde_json::to_string(value).unwrap_or_default()),
    }
}

fn estimate_context_entry_tokens(payload: &Value) -> u64 {
    if let Some(messages) = payload.get("messages").and_then(Value::as_array) {
        return messages.iter().map(estimate_tokens_from_value).sum();
    }

    if let Some(input) = payload.get("input").and_then(Value::as_array) {
        return input.iter().map(estimate_tokens_from_value).sum();
    }

    0
}

fn estimate_payload_baseline_tokens(payload: &Value) -> u64 {
    let mut base = payload.clone();
    if let Some(object) = base.as_object_mut() {
        object.remove("messages");
        object.remove("input");
    }
    estimate_tokens_from_value(&base)
}

fn allowed_input_tokens(
    context_limit: u32,
    requested_output_tokens: u64,
    output_reserve: u32,
) -> u64 {
    let limit = u64::from(context_limit.max(2));
    let reserved = requested_output_tokens
        .max(u64::from(output_reserve))
        .min(limit.saturating_sub(1));
    limit.saturating_sub(reserved)
}

fn entry_role(entry: &Value) -> Option<&str> {
    entry.get("role").and_then(Value::as_str)
}

fn entry_type(entry: &Value) -> Option<&str> {
    entry.get("type").and_then(Value::as_str)
}

fn entry_is_system(entry: &Value) -> bool {
    matches!(entry_role(entry), Some("system" | "developer"))
}

#[derive(Default)]
struct ContextProtection {
    protected: HashSet<usize>,
    compactable_tool_results: HashSet<usize>,
}

#[derive(Default)]
struct ToolEntryReferences {
    ids: Vec<String>,
    present: bool,
    malformed: bool,
}

impl ToolEntryReferences {
    fn record_id(&mut self, id: Option<&Value>) {
        self.present = true;
        match id.and_then(Value::as_str) {
            Some(id) if !id.trim().is_empty() => self.ids.push(id.to_owned()),
            _ => self.malformed = true,
        }
    }

    fn require_payload(&mut self, payload: Option<&Value>) {
        if payload.is_none_or(Value::is_null) {
            self.malformed = true;
        }
    }

    fn require_string_payload(&mut self, payload: Option<&Value>) {
        if payload
            .and_then(Value::as_str)
            .is_none_or(|value| value.trim().is_empty())
        {
            self.malformed = true;
        }
    }

    fn require_object_payload(&mut self, payload: Option<&Value>) {
        if !payload.is_some_and(Value::is_object) {
            self.malformed = true;
        }
    }
}

fn tool_call_references(entry: &Value) -> ToolEntryReferences {
    let mut references = ToolEntryReferences::default();
    if entry_type(entry) == Some("function_call") {
        references.record_id(entry.get("call_id"));
        references.require_string_payload(entry.get("arguments"));
    }
    if let Some(tool_calls) = entry.get("tool_calls").filter(|value| !value.is_null()) {
        match tool_calls.as_array() {
            Some(tool_calls) => {
                for call in tool_calls {
                    references.record_id(call.get("id"));
                    let function = call.get("function");
                    references
                        .require_string_payload(function.and_then(|value| value.get("arguments")));
                }
            }
            None => {
                references.present = true;
                references.malformed = true;
            }
        }
    }
    if let Some(blocks) = entry.get("content").and_then(Value::as_array) {
        for block in blocks {
            if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                references.record_id(block.get("id"));
                references.require_object_payload(block.get("input"));
            }
        }
    }
    references
}

fn tool_result_references(entry: &Value) -> ToolEntryReferences {
    let mut references = ToolEntryReferences::default();
    if entry_type(entry) == Some("function_call_output") {
        references.record_id(entry.get("call_id"));
        references.require_payload(entry.get("output"));
    }
    if entry_role(entry) == Some("tool") {
        references.record_id(entry.get("tool_call_id"));
        references.require_payload(entry.get("content"));
    }
    if entry_type(entry) == Some("tool_result") {
        references.record_id(entry.get("tool_use_id"));
        references.require_payload(entry.get("content"));
    }
    if let Some(blocks) = entry.get("content").and_then(Value::as_array) {
        for block in blocks {
            if block.get("type").and_then(Value::as_str) == Some("tool_result") {
                references.record_id(block.get("tool_use_id"));
                references.require_payload(block.get("content"));
            }
        }
    }
    references
}

fn entry_contains_reasoning(entry: &Value) -> bool {
    matches!(
        entry_type(entry),
        Some("reasoning" | "thinking" | "redacted_thinking")
    ) || entry.get("reasoning_content").is_some()
        || entry
            .get("content")
            .and_then(Value::as_array)
            .is_some_and(|blocks| {
                blocks.iter().any(|block| {
                    matches!(
                        block.get("type").and_then(Value::as_str),
                        Some("reasoning" | "thinking" | "redacted_thinking")
                    )
                })
            })
}

fn entry_is_plain_conversation(entry: &Value) -> bool {
    let calls = tool_call_references(entry);
    let results = tool_result_references(entry);
    (matches!(entry_role(entry), Some("user" | "assistant"))
        || entry_type(entry) == Some("message"))
        && !calls.present
        && !results.present
        && !entry_contains_reasoning(entry)
}

fn analyze_context_entries(entries: &[Value]) -> ContextProtection {
    let mut protection = ContextProtection::default();
    let recent_start = entries.len().saturating_sub(CONTEXT_KEEP_RECENT_ITEMS);
    protection.protected.extend(recent_start..entries.len());

    let call_references = entries.iter().map(tool_call_references).collect::<Vec<_>>();
    let result_references = entries
        .iter()
        .map(tool_result_references)
        .collect::<Vec<_>>();
    let mut calls = HashMap::<String, Vec<usize>>::new();
    let mut results = HashMap::<String, Vec<usize>>::new();
    for (index, entry) in entries.iter().enumerate() {
        if entry_is_system(entry) || entry_contains_reasoning(entry) {
            protection.protected.insert(index);
        }
        if call_references[index].present {
            protection.protected.insert(index);
        }
        for call_id in &call_references[index].ids {
            calls.entry(call_id.clone()).or_default().push(index);
        }
        for result_id in &result_references[index].ids {
            results.entry(result_id.clone()).or_default().push(index);
        }
    }

    if let Some(current_input) = entries
        .iter()
        .rposition(|entry| entry_role(entry) == Some("user"))
    {
        protection.protected.insert(current_input);
    }

    for (index, references) in result_references.iter().enumerate() {
        if !references.present {
            continue;
        }
        let uniquely_completed = !references.malformed
            && references.ids.iter().all(|call_id| {
                let Some(call_indices) = calls.get(call_id) else {
                    return false;
                };
                let Some(result_indices) = results.get(call_id) else {
                    return false;
                };
                call_indices.len() == 1
                    && result_indices.len() == 1
                    && call_indices[0] < index
                    && !call_references[call_indices[0]].malformed
            });
        if uniquely_completed && !protection.protected.contains(&index) {
            protection.compactable_tool_results.insert(index);
        } else {
            protection.protected.insert(index);
        }
    }

    for (index, entry) in entries.iter().enumerate() {
        if !protection.compactable_tool_results.contains(&index)
            && !entry_is_plain_conversation(entry)
        {
            protection.protected.insert(index);
        }
    }
    protection
}

fn summarize_text(text: &str, max_chars: usize, label: &str) -> String {
    let chars = text.chars().collect::<Vec<_>>();
    if chars.len() <= max_chars {
        return text.to_string();
    }
    let clip = max_chars.max(16);
    let head_size = clip / 2;
    let tail_size = clip.saturating_sub(head_size);
    let head = chars
        .iter()
        .take(head_size)
        .collect::<String>()
        .replace('\n', " ");
    let tail = chars
        .iter()
        .skip(chars.len().saturating_sub(tail_size))
        .collect::<String>()
        .replace('\n', " ");
    format!(
        "[gateway-summary {label} original_chars={} head=\"{}\" tail=\"{}\"]",
        chars.len(),
        head.trim(),
        tail.trim()
    )
}

fn value_to_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        _ => serde_json::to_string(value).unwrap_or_default(),
    }
}

fn truncate_value_field(value: &mut Value, max_chars: usize, label: &str) -> bool {
    let text = value_to_text(value);
    if text.chars().count() <= max_chars {
        return false;
    }
    replace_if_smaller(
        value,
        Value::String(summarize_text(&text, max_chars, label)),
    )
}

fn replace_if_smaller(value: &mut Value, replacement: Value) -> bool {
    if replacement == *value
        || estimate_tokens_from_value(&replacement) >= estimate_tokens_from_value(value)
    {
        return false;
    }
    *value = replacement;
    true
}

fn truncate_entry_content(
    entry: &mut Value,
    max_chars: usize,
    label: &str,
    tool_result: bool,
) -> bool {
    let Some(object) = entry.as_object_mut() else {
        return truncate_value_field(entry, max_chars, label);
    };

    if tool_result {
        if let Some(blocks) = object.get_mut("content").and_then(Value::as_array_mut) {
            let mut found_nested_result = false;
            let mut changed = false;
            for block in blocks {
                if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                    continue;
                }
                found_nested_result = true;
                if let Some(content) = block.get_mut("content") {
                    changed |= truncate_value_field(content, max_chars, label);
                }
            }
            if found_nested_result {
                return changed;
            }
        }
    }

    if let Some(content) = object.get_mut("content") {
        if truncate_value_field(content, max_chars, label) {
            return true;
        }
    }
    if let Some(output) = object.get_mut("output") {
        if truncate_value_field(output, max_chars, label) {
            return true;
        }
    }
    false
}

fn compact_entry(entry: &mut Value, tool_result: bool) -> bool {
    let label = if tool_result {
        "tool_result"
    } else {
        "history_message"
    };
    let summary = format!("[gateway-summary {label} omitted]");

    let Some(object) = entry.as_object_mut() else {
        return replace_if_smaller(entry, Value::String(summary));
    };

    if tool_result {
        if let Some(output) = object.get_mut("output") {
            return replace_if_smaller(output, Value::String(summary));
        }
        if let Some(blocks) = object.get_mut("content").and_then(Value::as_array_mut) {
            let mut found_nested_result = false;
            let mut changed = false;
            for block in blocks {
                if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                    continue;
                }
                found_nested_result = true;
                if let Some(content) = block.get_mut("content") {
                    changed |= replace_if_smaller(content, Value::String(summary.clone()));
                }
            }
            if found_nested_result {
                return changed;
            }
        }
    }
    if let Some(content) = object.get_mut("content") {
        return replace_if_smaller(content, Value::String(summary));
    }
    if let Some(output) = object.get_mut("output") {
        return replace_if_smaller(output, Value::String(summary));
    }
    false
}

fn estimate_entries_tokens(entries: &[Value]) -> u64 {
    entries.iter().map(estimate_tokens_from_value).sum()
}

fn trim_entries_to_budget(
    entries: &mut [Value],
    target_tokens: u64,
    protection: &ContextProtection,
) -> ContextTrimStats {
    let mut stats = ContextTrimStats::default();
    if entries.is_empty() {
        return stats;
    }

    let mut candidates = protection
        .compactable_tool_results
        .iter()
        .copied()
        .collect::<Vec<_>>();
    candidates.sort_unstable();
    candidates.extend((0..entries.len()).filter(|index| {
        !protection.protected.contains(index)
            && !protection.compactable_tool_results.contains(index)
            && entry_is_plain_conversation(&entries[*index])
    }));

    let mut current_tokens = estimate_entries_tokens(entries);

    for index in &candidates {
        if current_tokens <= target_tokens {
            break;
        }
        let tool_result = protection.compactable_tool_results.contains(index);
        let max_chars = if tool_result {
            CONTEXT_TOOL_RESULT_TRUNCATE_CHARS
        } else {
            CONTEXT_MESSAGE_TRUNCATE_CHARS
        };
        let label = if tool_result {
            "tool_result"
        } else {
            "message"
        };
        if truncate_entry_content(&mut entries[*index], max_chars, label, tool_result) {
            stats.truncated_blocks = stats.truncated_blocks.saturating_add(1);
            if tool_result {
                stats.tool_result_blocks = stats.tool_result_blocks.saturating_add(1);
            }
            current_tokens = estimate_entries_tokens(entries);
        }
    }

    for index in &candidates {
        if current_tokens <= target_tokens {
            break;
        }
        let tool_result = protection.compactable_tool_results.contains(index);
        if compact_entry(&mut entries[*index], tool_result) {
            stats.compacted_entries = stats.compacted_entries.saturating_add(1);
            if tool_result {
                stats.tool_result_blocks = stats.tool_result_blocks.saturating_add(1);
            }
            current_tokens = estimate_entries_tokens(entries);
        }
    }

    stats
}

fn context_entries(payload: &Value) -> Option<&[Value]> {
    payload
        .get("messages")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .or_else(|| {
            payload
                .get("input")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
        })
}

fn context_protection(payload: &Value) -> ContextProtection {
    context_entries(payload)
        .map(analyze_context_entries)
        .unwrap_or_default()
}

fn estimate_context_minimum_tokens(payload: &Value, protection: &ContextProtection) -> u64 {
    context_entries(payload)
        .unwrap_or_default()
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            if protection.protected.contains(&index) {
                return estimate_tokens_from_value(entry);
            }
            let original_tokens = estimate_tokens_from_value(entry);
            let mut minimum = entry.clone();
            let tool_result = protection.compactable_tool_results.contains(&index);
            if tool_result || entry_is_plain_conversation(entry) {
                compact_entry(&mut minimum, tool_result);
            }
            original_tokens.min(estimate_tokens_from_value(&minimum))
        })
        .sum()
}

fn trim_context_entries_with_protection(
    payload: &mut Value,
    target_tokens: u64,
    protection: &ContextProtection,
) -> ContextTrimStats {
    if let Some(messages) = payload.get_mut("messages").and_then(Value::as_array_mut) {
        return trim_entries_to_budget(messages, target_tokens, protection);
    }

    if let Some(input) = payload.get_mut("input").and_then(Value::as_array_mut) {
        return trim_entries_to_budget(input, target_tokens, protection);
    }

    ContextTrimStats::default()
}

fn trim_context_entries(payload: &mut Value, target_tokens: u64) -> ContextTrimStats {
    let protection = context_protection(payload);
    trim_context_entries_with_protection(payload, target_tokens, &protection)
}

pub(super) fn apply_context_budget_controls(
    upstream: &UpstreamConfig,
    global_context_profile: Option<&GlobalContextProfile>,
    payload: &mut Value,
    model: &str,
) -> Option<ContextBudgetReport> {
    let mut config =
        upstream.context_config_for_model_with_profile(model, global_context_profile)?;
    let requested_output_tokens = requested_output_tokens_from_payload(payload);
    let mut baseline_tokens = estimate_payload_baseline_tokens(payload);
    let mut entry_tokens = estimate_context_entry_tokens(payload);
    let mut context_limit = config.context_limit;
    let mut output_reserve = config.output_reserve;
    let mut allowed = allowed_input_tokens(context_limit, requested_output_tokens, output_reserve);
    let estimated_input_tokens = baseline_tokens.saturating_add(entry_tokens);
    let mut trim_stats = ContextTrimStats::default();
    let mut fallback_model = None;

    if estimated_input_tokens > allowed {
        let target_entry_tokens = allowed.saturating_sub(baseline_tokens);
        let stats = trim_context_entries(payload, target_entry_tokens);
        trim_stats.truncated_blocks = trim_stats
            .truncated_blocks
            .saturating_add(stats.truncated_blocks);
        trim_stats.compacted_entries = trim_stats
            .compacted_entries
            .saturating_add(stats.compacted_entries);
        trim_stats.tool_result_blocks = trim_stats
            .tool_result_blocks
            .saturating_add(stats.tool_result_blocks);

        baseline_tokens = estimate_payload_baseline_tokens(payload);
        entry_tokens = estimate_context_entry_tokens(payload);
    }

    let mut estimated_after_trim = baseline_tokens.saturating_add(entry_tokens);
    if estimated_after_trim > allowed {
        let required_limit = estimated_after_trim
            .saturating_add(requested_output_tokens.max(u64::from(output_reserve)))
            .min(u64::from(u32::MAX)) as u32;

        if let Some(switched_model) = upstream.context_fallback_model_for_with_profile(
            model,
            required_limit,
            global_context_profile,
        ) {
            if let Some(object) = payload.as_object_mut() {
                object.insert("model".into(), Value::String(switched_model.clone()));
            }
            fallback_model = Some(switched_model.clone());

            if let Some(next_config) = upstream
                .context_config_for_model_with_profile(&switched_model, global_context_profile)
            {
                config = next_config;
                context_limit = config.context_limit;
                output_reserve = config.output_reserve;
                allowed =
                    allowed_input_tokens(context_limit, requested_output_tokens, output_reserve);
            }

            if estimated_after_trim > allowed {
                let target_entry_tokens = allowed.saturating_sub(baseline_tokens);
                let stats = trim_context_entries(payload, target_entry_tokens);
                trim_stats.truncated_blocks = trim_stats
                    .truncated_blocks
                    .saturating_add(stats.truncated_blocks);
                trim_stats.compacted_entries = trim_stats
                    .compacted_entries
                    .saturating_add(stats.compacted_entries);
                trim_stats.tool_result_blocks = trim_stats
                    .tool_result_blocks
                    .saturating_add(stats.tool_result_blocks);

                baseline_tokens = estimate_payload_baseline_tokens(payload);
                entry_tokens = estimate_context_entry_tokens(payload);
                estimated_after_trim = baseline_tokens.saturating_add(entry_tokens);
            }
        }
    }

    // Clamp max_tokens / max_output_tokens / max_completion_tokens if the
    // upstream configured a `max_output_tokens` cap. This prevents sending
    // an excessively large generation budget (e.g. Codex's default 65536)
    // to upstreams that either don't support it or whose account balance
    // cannot cover it, which would result in 402 / 400 errors.
    let max_output_tokens_cap = config.max_output_tokens;
    let mut max_output_tokens_clamped = false;
    if max_output_tokens_cap > 0 {
        if let Some(object) = payload.as_object_mut() {
            for key in ["max_tokens", "max_output_tokens", "max_completion_tokens"] {
                if let Some(current) = object.get(key).and_then(Value::as_u64) {
                    if current > u64::from(max_output_tokens_cap) {
                        object.insert(key.to_string(), Value::Number(max_output_tokens_cap.into()));
                        max_output_tokens_clamped = true;
                    }
                }
            }
        }
    }

    let protection = context_protection(payload);
    let protected_minimum_tokens = estimate_payload_baseline_tokens(payload)
        .saturating_add(estimate_context_minimum_tokens(payload, &protection));

    Some(ContextBudgetReport {
        estimated_input_tokens,
        estimated_input_tokens_after_trim: estimated_after_trim,
        protected_minimum_tokens,
        compacted_items: trim_stats.compacted_entries,
        requested_output_tokens,
        allowed_input_tokens: allowed,
        context_limit,
        output_reserve,
        max_output_tokens_cap,
        max_output_tokens_clamped,
        trim_stats,
        fallback_model,
    })
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ContextOverflowRetryReport {
    pub(super) changed: bool,
    pub(super) protected_minimum_tokens: u64,
    pub(super) compacted_items: u32,
}

pub(super) fn compact_for_context_overflow_retry(
    payload: &mut Value,
    budget: &ContextBudgetReport,
) -> ContextOverflowRetryReport {
    let target = budget.allowed_input_tokens.saturating_mul(9) / 10;
    let baseline_tokens = estimate_payload_baseline_tokens(payload);
    let protection = context_protection(payload);
    let protected_minimum_tokens =
        baseline_tokens.saturating_add(estimate_context_minimum_tokens(payload, &protection));
    if protected_minimum_tokens > target {
        return ContextOverflowRetryReport {
            changed: false,
            protected_minimum_tokens,
            compacted_items: 0,
        };
    }

    let target_entry_tokens = target.saturating_sub(baseline_tokens);
    let stats = trim_context_entries_with_protection(payload, target_entry_tokens, &protection);
    let generation_changed = halve_generation_cap_for_context_retry(payload).is_some();
    ContextOverflowRetryReport {
        changed: generation_changed || stats.compacted_entries > 0 || stats.truncated_blocks > 0,
        protected_minimum_tokens,
        compacted_items: stats.compacted_entries,
    }
}

pub(super) fn halve_generation_cap_for_context_retry(
    payload: &mut Value,
) -> Option<(&'static str, u64, u64)> {
    let object = payload.as_object_mut()?;
    for key in ["max_output_tokens", "max_tokens", "max_completion_tokens"] {
        let Some(current) = object.get(key).and_then(Value::as_u64) else {
            continue;
        };
        if current <= 1 {
            continue;
        }
        let reduced = (current / 2).max(1);
        object.insert(key.to_string(), Value::Number(reduced.into()));
        return Some((key, current, reduced));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn messages_tool_result_compaction_preserves_pairing_identity() {
        let tool_input = json!({"path": "important.txt"});
        let mut payload = json!({
            "messages": [
                {"role": "system", "content": "system invariant"},
                {
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": "toolu-closed",
                        "name": "read_file",
                        "input": tool_input
                    }]
                },
                {
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "toolu-closed",
                        "content": "TOOL_OUTPUT ".repeat(900)
                    }]
                },
                {"role": "user", "content": "OLD_USER ".repeat(500)},
                {"role": "assistant", "content": "recent assistant 1"},
                {"role": "user", "content": "recent user 1"},
                {"role": "assistant", "content": "recent assistant 2"},
                {"role": "user", "content": "recent user 2"},
                {"role": "assistant", "content": "recent assistant 3"},
                {"role": "user", "content": "recent user 3"},
                {"role": "assistant", "content": "recent assistant 4"},
                {"role": "user", "content": "current input"}
            ]
        });

        trim_context_entries(&mut payload, 0);

        assert_eq!(payload["messages"][1]["content"][0]["input"], tool_input);
        assert_eq!(
            payload["messages"][2]["content"][0]["tool_use_id"],
            "toolu-closed"
        );
        assert!(payload["messages"][2]["content"][0]["content"]
            .as_str()
            .unwrap_or_default()
            .contains("[gateway-summary tool_result"));
    }

    #[test]
    fn malformed_or_payloadless_tool_entries_remain_unchanged() {
        let malformed_missing_id = json!({
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "name": "read_file",
                "input": {"path": "MISSING_ID ".repeat(500)}
            }]
        });
        let malformed_non_string_id = json!({
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": 7,
                "name": "read_file",
                "input": {"path": "NON_STRING_ID ".repeat(500)}
            }]
        });
        let malformed_empty_id = json!({
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "",
                "name": "read_file",
                "input": {"path": "EMPTY_ID ".repeat(500)}
            }]
        });
        let mixed_results = json!({
            "role": "user",
            "content": [
                {
                    "type": "tool_result",
                    "tool_use_id": "closed-call",
                    "content": "VALID_RESULT ".repeat(500)
                },
                {
                    "type": "tool_result",
                    "content": "MISSING_RESULT_ID ".repeat(500)
                }
            ]
        });
        let payloadless_result = json!({
            "type": "function_call_output",
            "call_id": "payloadless-call"
        });
        let responses_call_without_arguments = json!({
            "type": "function_call",
            "call_id": "responses-call-without-arguments",
            "name": "read_file"
        });
        let responses_result_for_payloadless_call = json!({
            "type": "function_call_output",
            "call_id": "responses-call-without-arguments",
            "output": "RESPONSES_RESULT ".repeat(500)
        });
        let chat_call_with_null_arguments = json!({
            "role": "assistant",
            "tool_calls": [{
                "id": "chat-call-without-arguments",
                "type": "function",
                "function": {"name": "read_file", "arguments": null}
            }]
        });
        let chat_result_for_payloadless_call = json!({
            "role": "tool",
            "tool_call_id": "chat-call-without-arguments",
            "content": "CHAT_RESULT ".repeat(500)
        });
        let messages_call_with_invalid_input = json!({
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "messages-call-without-input",
                "name": "read_file",
                "input": "not-an-object"
            }]
        });
        let messages_result_for_payloadless_call = json!({
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "messages-call-without-input",
                "content": "MESSAGES_RESULT ".repeat(500)
            }]
        });
        let mut payload = json!({
            "messages": [
                {"role": "system", "content": "system invariant"},
                malformed_missing_id,
                malformed_non_string_id,
                malformed_empty_id,
                {
                    "type": "function_call",
                    "call_id": "closed-call",
                    "name": "read_file",
                    "arguments": "{}"
                },
                mixed_results,
                {
                    "type": "function_call",
                    "call_id": "payloadless-call",
                    "name": "read_file",
                    "arguments": "{}"
                },
                payloadless_result,
                responses_call_without_arguments,
                responses_result_for_payloadless_call,
                chat_call_with_null_arguments,
                chat_result_for_payloadless_call,
                messages_call_with_invalid_input,
                messages_result_for_payloadless_call,
                {"role": "assistant", "content": "recent assistant 1"},
                {"role": "user", "content": "recent user 1"},
                {"role": "assistant", "content": "recent assistant 2"},
                {"role": "user", "content": "recent user 2"},
                {"role": "assistant", "content": "recent assistant 3"},
                {"role": "user", "content": "recent user 3"},
                {"role": "assistant", "content": "recent assistant 4"},
                {"role": "user", "content": "current input"}
            ]
        });
        let original = payload.clone();

        trim_context_entries(&mut payload, 0);

        for index in [1, 2, 3, 5, 7, 9, 11, 13] {
            assert_eq!(payload["messages"][index], original["messages"][index]);
        }
        assert!(payload["messages"][7].get("content").is_none());
    }

    #[test]
    fn duplicate_tool_ids_keep_all_results_uncompacted() {
        let mut payload = json!({
            "input": [
                {"role": "system", "content": "system invariant"},
                {"type": "function_call", "call_id": "duplicate-call", "name": "a", "arguments": "{}"},
                {"type": "function_call", "call_id": "duplicate-call", "name": "b", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "duplicate-call", "output": "AMBIGUOUS ".repeat(500)},
                {"type": "function_call", "call_id": "duplicate-result", "name": "c", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "duplicate-result", "output": "FIRST_RESULT ".repeat(500)},
                {"type": "function_call_output", "call_id": "duplicate-result", "output": "SECOND_RESULT ".repeat(500)},
                {"role": "assistant", "content": "recent assistant 1"},
                {"role": "user", "content": "recent user 1"},
                {"role": "assistant", "content": "recent assistant 2"},
                {"role": "user", "content": "recent user 2"},
                {"role": "assistant", "content": "recent assistant 3"},
                {"role": "user", "content": "recent user 3"},
                {"role": "assistant", "content": "recent assistant 4"},
                {"role": "user", "content": "current input"}
            ]
        });
        let original = payload.clone();

        trim_context_entries(&mut payload, 0);

        for index in [3, 5, 6] {
            assert_eq!(payload["input"][index], original["input"][index]);
        }
    }

    #[test]
    fn context_minimum_never_inflates_short_candidates() {
        let payload = json!({
            "messages": [
                {"role": "system", "content": "system invariant"},
                {"role": "user", "content": "x"},
                {"role": "assistant", "content": "y"},
                {"role": "assistant", "content": "recent assistant 1"},
                {"role": "user", "content": "recent user 1"},
                {"role": "assistant", "content": "recent assistant 2"},
                {"role": "user", "content": "recent user 2"},
                {"role": "assistant", "content": "recent assistant 3"},
                {"role": "user", "content": "recent user 3"},
                {"role": "assistant", "content": "recent assistant 4"},
                {"role": "user", "content": "current input"}
            ]
        });
        let protection = context_protection(&payload);

        assert!(
            estimate_context_minimum_tokens(&payload, &protection)
                <= estimate_context_entry_tokens(&payload)
        );
    }
}
