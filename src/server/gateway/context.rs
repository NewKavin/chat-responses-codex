use crate::state::{GlobalContextProfile, UpstreamConfig};
use serde_json::Value;
use std::collections::HashSet;

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

fn entry_is_tool_result(entry: &Value) -> bool {
    matches!(entry_role(entry), Some("tool" | "function"))
        || matches!(
            entry_type(entry),
            Some("function_call_output" | "tool_result")
        )
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
    *value = Value::String(summarize_text(&text, max_chars, label));
    true
}

fn truncate_entry_content(entry: &mut Value, max_chars: usize, label: &str) -> bool {
    let Some(object) = entry.as_object_mut() else {
        return truncate_value_field(entry, max_chars, label);
    };

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
    if let Some(arguments) = object.get_mut("arguments") {
        if truncate_value_field(arguments, max_chars, label) {
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
        *entry = Value::String(summary);
        return true;
    };

    if tool_result {
        if let Some(output) = object.get_mut("output") {
            *output = Value::String(summary);
            return true;
        }
    }
    if let Some(content) = object.get_mut("content") {
        *content = Value::String(summary);
        return true;
    }
    if let Some(output) = object.get_mut("output") {
        *output = Value::String(summary);
        return true;
    }

    object.insert("content".into(), Value::String(summary));
    true
}

fn estimate_entries_tokens(entries: &[Value]) -> u64 {
    entries.iter().map(estimate_tokens_from_value).sum()
}

fn trim_entries_to_budget(entries: &mut [Value], target_tokens: u64) -> ContextTrimStats {
    let mut stats = ContextTrimStats::default();
    if entries.is_empty() {
        return stats;
    }

    let keep_recent_start = entries.len().saturating_sub(CONTEXT_KEEP_RECENT_ITEMS);
    let mut protected = HashSet::new();
    for index in keep_recent_start..entries.len() {
        protected.insert(index);
    }
    for (index, entry) in entries.iter().enumerate() {
        if entry_is_system(entry) {
            protected.insert(index);
        }
    }

    let mut candidates = (0..entries.len())
        .filter(|index| !protected.contains(index))
        .collect::<Vec<_>>();
    candidates.sort_by_key(|index| (!entry_is_tool_result(&entries[*index]), *index));

    let mut current_tokens = estimate_entries_tokens(entries);

    for index in &candidates {
        if current_tokens <= target_tokens {
            break;
        }
        let tool_result = entry_is_tool_result(&entries[*index]);
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
        if truncate_entry_content(&mut entries[*index], max_chars, label) {
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
        let tool_result = entry_is_tool_result(&entries[*index]);
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

fn trim_context_entries(payload: &mut Value, target_tokens: u64) -> ContextTrimStats {
    if let Some(messages) = payload.get_mut("messages").and_then(Value::as_array_mut) {
        return trim_entries_to_budget(messages, target_tokens);
    }

    if let Some(input) = payload.get_mut("input").and_then(Value::as_array_mut) {
        return trim_entries_to_budget(input, target_tokens);
    }

    ContextTrimStats::default()
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

    Some(ContextBudgetReport {
        estimated_input_tokens,
        estimated_input_tokens_after_trim: estimated_after_trim,
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
