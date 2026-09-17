use super::{DownstreamConfig, ModelAccessSelection};
use serde_json::{Map, Value};

pub fn parse_model_access_update(updates: &Map<String, Value>) -> Result<Option<ModelAccessSelection>, String> {
    match (updates.get("model_access"), updates.get("model_group_id")) {
        (Some(_),Some(_)) => Err("specify model_access or model_group_id, not both".into()),
        (Some(value),None) => {
            let selection: ModelAccessSelection = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
            selection.validate()?;
            Ok(Some(selection))
        }
        (None,Some(Value::String(id))) if !id.trim().is_empty() => Ok(Some(ModelAccessSelection::from_group_id(id.trim()))),
        (None,Some(_)) => Err("model_group_id must be nonempty; use model_access.mode=inherit explicitly".into()),
        (None,None) => Ok(None),
    }
}

pub fn apply_downstream_updates(
    downstream: &mut DownstreamConfig,
    updates: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    if updates.is_empty() { return Err("updates must not be empty".into()); }
    for field in ["active", "rate_limit_enabled"] {
        if updates.get(field).is_some_and(|value| !value.is_boolean()) {
            return Err(format!("{field} must be a boolean"));
        }
    }
    for field in ["per_minute_limit", "max_concurrency", "request_quota_window_hours", "request_quota_requests", "daily_token_limit", "monthly_token_limit", "input_token_price_per_million_cents", "output_token_price_per_million_cents"] {
        if let Some(value) = updates.get(field) {
            if value.is_null() && !matches!(field,"per_minute_limit"|"max_concurrency") { continue; }
            let number = value.as_u64().ok_or_else(|| format!("{field} must be a nonnegative integer"))?;
            if number > i64::MAX as u64 || (matches!(field,"per_minute_limit"|"max_concurrency"|"request_quota_window_hours"|"request_quota_requests") && number > u32::MAX as u64) {
                return Err(format!("{field} is out of range"));
            }
            if field == "request_quota_window_hours" && !(1..=168).contains(&number) {
                return Err("request_quota_window_hours must be between 1 and 168".into());
            }
        }
    }
    if let Some(value) = updates.get("expires_at") {
        downstream.expires_at = if value.is_null() { None } else {
            let expiry = value.as_u64().filter(|value| *value <= 253_402_300_799)
                .ok_or_else(|| "expires_at must be Unix seconds or null".to_owned())?;
            Some(expiry)
        };
    }
    if let Some(name) = updates.get("name").and_then(|v| v.as_str()) {
        downstream.name = name.to_string();
    }
    if let Some(per_minute_limit) = updates.get("per_minute_limit").and_then(|v| v.as_u64()) {
        downstream.per_minute_limit = per_minute_limit as u32;
    }
    if let Some(max_concurrency) = updates.get("max_concurrency").and_then(|v| v.as_u64()) {
        downstream.max_concurrency = max_concurrency as u32;
    }
    if let Some(rate_limit_enabled) = updates.get("rate_limit_enabled").and_then(|v| v.as_bool()) {
        downstream.rate_limit_enabled = rate_limit_enabled;
    }
    if let Some(billing_mode) = updates.get("billing_mode").and_then(|v| v.as_str()) {
        if billing_mode != "request" && billing_mode != "token" {
            return Err("billing_mode must be \"request\" or \"token\"".to_string());
        }
        downstream.billing_mode = billing_mode.to_string();
    }
    if let Some(request_quota_window_hours) = updates
        .get("request_quota_window_hours")
        .and_then(|v| v.as_u64())
    {
        downstream.request_quota_window_hours = Some(request_quota_window_hours as u32);
    }
    if updates
        .get("request_quota_window_hours")
        .is_some_and(serde_json::Value::is_null)
    {
        downstream.request_quota_window_hours = None;
    }
    if let Some(request_quota_requests) = updates
        .get("request_quota_requests")
        .and_then(|v| v.as_u64())
    {
        downstream.request_quota_requests = Some(request_quota_requests as u32);
    }
    if updates
        .get("request_quota_requests")
        .is_some_and(serde_json::Value::is_null)
    {
        downstream.request_quota_requests = None;
    }
    // Legacy allowlists are retained for migration, never updated by this API.
    if updates.get("model_allowlist").is_some() {
        tracing::warn!(
            downstream_id = %downstream.id,
            "ignoring model_allowlist in update (model groups are the single source of truth)"
        );
    }
    if let Some(ip_allowlist) = updates.get("ip_allowlist").and_then(|v| v.as_array()) {
        downstream.ip_allowlist = ip_allowlist
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
    }
    if let Some(daily_token_limit) = updates.get("daily_token_limit").and_then(|v| v.as_u64()) {
        downstream.daily_token_limit = Some(daily_token_limit);
    }
    if updates
        .get("daily_token_limit")
        .is_some_and(serde_json::Value::is_null)
    {
        downstream.daily_token_limit = None;
    }
    if let Some(monthly_token_limit) = updates.get("monthly_token_limit").and_then(|v| v.as_u64()) {
        downstream.monthly_token_limit = Some(monthly_token_limit);
    }
    if updates
        .get("monthly_token_limit")
        .is_some_and(serde_json::Value::is_null)
    {
        downstream.monthly_token_limit = None;
    }
    if let Some(price) = updates
        .get("input_token_price_per_million_cents")
        .and_then(|v| v.as_u64())
    {
        downstream.input_token_price_per_million_cents = Some(price);
    }
    if updates
        .get("input_token_price_per_million_cents")
        .is_some_and(serde_json::Value::is_null)
    {
        downstream.input_token_price_per_million_cents = None;
    }
    if let Some(price) = updates
        .get("output_token_price_per_million_cents")
        .and_then(|v| v.as_u64())
    {
        downstream.output_token_price_per_million_cents = Some(price);
    }
    if updates
        .get("output_token_price_per_million_cents")
        .is_some_and(serde_json::Value::is_null)
    {
        downstream.output_token_price_per_million_cents = None;
    }
    if let Some(active) = updates.get("active").and_then(|v| v.as_bool()) {
        downstream.active = active;
    }
    if let Some(groups_value) = updates.get("model_concurrency_groups") {
        if groups_value.is_null() {
            downstream.model_concurrency_groups = Vec::new();
        } else {
            let groups = serde_json::from_value::<Vec<crate::state::ModelConcurrencyGroup>>(
                groups_value.clone(),
            )
            .map_err(|_| {
                "model_concurrency_groups must be an array of objects {\"name\", \"match\", \"max_concurrency\"}"
                    .to_string()
            })?;
            let mut candidate = downstream.clone();
            candidate.model_concurrency_groups = groups;
            candidate
                .validate_model_concurrency_groups()
                .map_err(|message| message.to_string())?;
            let cap_sum: u64 = candidate
                .model_concurrency_groups
                .iter()
                .map(|group| u64::from(group.max_concurrency))
                .sum();
            if cap_sum > u64::from(downstream.max_concurrency.max(1)) {
                tracing::warn!(
                    downstream_id = %downstream.id,
                    cap_sum,
                    global_max_concurrency = downstream.max_concurrency,
                    "model_concurrency_groups caps sum above the global max_concurrency (legal overbooking; the global cap stays the backstop)"
                );
            }
            downstream.model_concurrency_groups = candidate.model_concurrency_groups;
        }
    }
    Ok(())
}
