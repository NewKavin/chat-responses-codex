use crate::state::AppState;
use super::normalize::{parse_u64_flexible, parse_upstream_protocol, parse_upstream_protocols};
use serde_json::Value;
use super::types::*;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use uuid::Uuid;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FreekeySyncSummary {
    pub created: usize,
    pub updated: usize,
    pub skipped: usize,
}

#[derive(Debug, Clone)]
pub struct FreekeySyncItem {
    pub name: Option<String>,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

pub(super) fn merge_api_keys(existing: &[String], incoming: &[String]) -> Vec<String> {
    let mut keys: Vec<String> = existing.iter().cloned().collect();
    let mut seen: HashSet<String> = existing.iter().cloned().collect();
    for key in incoming {
        let key = key.trim().to_string();
        if key.is_empty() {
            continue;
        }
        if seen.insert(key.clone()) {
            keys.push(key);
        }
    }
    keys
}

pub(super) fn merge_api_key_models(
    existing: &[ApiKeyModelConfig],
    incoming: &[ApiKeyModelConfig],
) -> Vec<ApiKeyModelConfig> {
    let mut merged: Vec<ApiKeyModelConfig> = existing.to_vec();
    for new_item in incoming {
        let new_key = new_item.api_key.trim().to_string();
        if new_key.is_empty() {
            continue;
        }
        if let Some(existing_item) = merged.iter_mut().find(|e| e.api_key.trim() == new_key) {
            for model in &new_item.supported_models {
                let model = model.trim().to_string();
                if !model.is_empty()
                    && !existing_item
                        .supported_models
                        .iter()
                        .any(|m| m.trim() == model)
                {
                    existing_item.supported_models.push(model);
                }
            }
        } else {
            let supported_models: Vec<String> = new_item
                .supported_models
                .iter()
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty())
                .collect();
            if !supported_models.is_empty() || !new_key.is_empty() {
                merged.push(ApiKeyModelConfig {
                    api_key: new_key,
                    supported_models,
                });
            }
        }
    }
    merged
}

pub(super) fn derive_supported_models(key_models: &[ApiKeyModelConfig]) -> Vec<String> {
    let mut models = Vec::new();
    let mut seen = HashSet::new();
    for item in key_models {
        for model in &item.supported_models {
            let model = model.trim().to_string();
            if !model.is_empty() && seen.insert(model.clone()) {
                models.push(model);
            }
        }
    }
    models
}

impl AppState {
    pub async fn sync_freekey_upstreams(
        &self,
        source: String,
        imports: Vec<FreekeySyncItem>,
        synced_at: u64,
    ) -> Result<FreekeySyncSummary, String> {
        let source = source.trim().to_string();
        let imports = imports
            .into_iter()
            .filter_map(|item| {
                let base_url = item.base_url.trim().to_string();
                let api_key = item.api_key.trim().to_string();
                let model = item.model.trim().to_string();
                let name = item.name.and_then(|value| {
                    let trimmed = value.trim().to_string();
                    if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed)
                    }
                });

                if base_url.is_empty() || api_key.is_empty() || model.is_empty() {
                    return None;
                }

                Some(FreekeySyncItem {
                    name,
                    base_url,
                    api_key,
                    model,
                })
            })
            .collect::<Vec<_>>();

        if imports.is_empty() {
            return Ok(FreekeySyncSummary::default());
        }

        let mut grouped: Vec<(String, Vec<FreekeySyncItem>)> = Vec::new();
        for item in imports.into_iter() {
            if let Some(group) = grouped
                .iter_mut()
                .find(|(base_url, _)| base_url == &item.base_url)
            {
                group.1.push(item);
            } else {
                grouped.push((item.base_url.clone(), vec![item]));
            }
        }

        self.mutate_persisted_state(
            |candidate_state| {
                let mut result = FreekeySyncSummary::default();

                let find_auto_upstream = |state: &[UpstreamConfig], base_url: &str| {
                    state
                        .iter()
                        .position(|upstream| upstream.base_url == base_url && upstream.auto_managed)
                };

                let has_manual_upstream = |state: &[UpstreamConfig], base_url: &str| {
                    state
                        .iter()
                        .any(|upstream| upstream.base_url == base_url && !upstream.auto_managed)
                };

                let fold_group = |items: &[FreekeySyncItem]| {
                    let mut keys: Vec<String> = Vec::new();
                    let mut models: Vec<String> = Vec::new();
                    let mut key_models: Vec<ApiKeyModelConfig> = Vec::new();
                    let mut last_name: Option<String> = None;
                    for item in items {
                        let key = item.api_key.trim().to_string();
                        let model = item.model.trim().to_string();
                        if !key.is_empty() && !keys.iter().any(|k| k == &key) {
                            keys.push(key.clone());
                        }
                        if !model.is_empty() && !models.iter().any(|m| m == &model) {
                            models.push(model.clone());
                        }
                        if !key.is_empty() && !model.is_empty() {
                            if let Some(entry) =
                                key_models.iter_mut().find(|entry| entry.api_key == key)
                            {
                                if !entry.supported_models.iter().any(|m| m == &model) {
                                    entry.supported_models.push(model.clone());
                                }
                            } else {
                                key_models.push(ApiKeyModelConfig {
                                    api_key: key.clone(),
                                    supported_models: vec![model.clone()],
                                });
                            }
                        }
                        if let Some(name) = &item.name {
                            last_name = Some(name.clone());
                        }
                    }
                    (keys, models, key_models, last_name)
                };

                for (base_url, items) in grouped.iter() {
                    if has_manual_upstream(&candidate_state.upstreams, base_url) {
                        result.skipped = result.skipped.saturating_add(items.len());
                        continue;
                    }

                    let (keys, models, key_models, last_name) = fold_group(items);
                    if keys.is_empty() || models.is_empty() {
                        result.skipped = result.skipped.saturating_add(items.len());
                        continue;
                    }

                    if let Some(index) = find_auto_upstream(&candidate_state.upstreams, base_url) {
                        let upstream = &mut candidate_state.upstreams[index];
                        upstream.api_keys = merge_api_keys(&upstream.api_keys, &keys);
                        upstream.api_key_models =
                            merge_api_key_models(&upstream.api_key_models, &key_models);
                        let mut merged_models: Vec<String> =
                            derive_supported_models(&upstream.api_key_models);
                        for model in upstream.supported_models.iter().chain(models.iter()) {
                            let model = model.trim().to_string();
                            if !model.is_empty() && !merged_models.iter().any(|m| m == &model) {
                                merged_models.push(model);
                            }
                        }
                        upstream.supported_models = merged_models;
                        if let Some(name) = last_name {
                            upstream.name = name;
                        }
                        upstream.managed_source = Some(source.clone());
                        upstream.last_synced_at = synced_at;
                        upstream.normalize_for_storage();

                        result.updated = result.updated.saturating_add(items.len());
                        continue;
                    }

                    let primary_key = keys.first().cloned().unwrap_or_default();
                    let extra_keys: Vec<String> = keys.iter().skip(1).cloned().collect();
                    let primary_model = models.first().cloned().unwrap_or_default();
                    let mut upstream = UpstreamConfig {
                        id: Uuid::new_v4().to_string(),
                        name: last_name.unwrap_or(primary_model),
                        base_url: base_url.clone(),
                        api_key: primary_key,
                        api_keys: extra_keys,
                        api_key_models: key_models,
                        supported_models: models,
                        auto_managed: true,
                        managed_source: Some(source.clone()),
                        last_synced_at: synced_at,
                        active: true,
                        ..Default::default()
                    };
                    upstream.normalize_for_storage();
                    candidate_state.upstreams.push(upstream);
                    result.created = result.created.saturating_add(items.len());
                }

                Ok(result)
            },
            |error| format!("Failed to persist state: {}", error),
        )
        .await
    }

    pub async fn update_upstream_by_id(
        &self,
        id: &str,
        updates: serde_json::Value,
    ) -> Result<UpstreamConfig, UpstreamMutationError> {
        self.mutate_persisted_state(
            |candidate_state| {
                let upstream = candidate_state
                    .upstreams
                    .iter_mut()
                    .find(|u| u.id == id)
                    .ok_or_else(|| {
                        UpstreamMutationError::NotFound(format!("Upstream '{}' not found", id))
                    })?;

                if let Some(name) = updates.get("name").and_then(|v| v.as_str()) {
                    upstream.name = name.to_string();
                }
                if let Some(base_url) = updates.get("base_url").and_then(|v| v.as_str()) {
                    upstream.base_url = base_url.to_string();
                }
                if let Some(api_key) = updates.get("api_key").and_then(|v| v.as_str()) {
                    let new_key = api_key.to_string();
                    if !new_key.is_empty() {
                        let mut merged = merge_api_keys(&upstream.api_keys, &[new_key.clone()]);
                        if !upstream.api_key.is_empty()
                            && !merged.iter().any(|k| k == &upstream.api_key)
                        {
                            merged.insert(0, upstream.api_key.clone());
                        }
                        upstream.api_keys = merged;
                    }
                }
                if let Some(api_keys) = updates.get("api_keys").and_then(|v| v.as_array()) {
                    let incoming: Vec<String> = api_keys
                        .iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect();
                    upstream.api_keys = merge_api_keys(&upstream.api_keys, &incoming);
                }
                if let Some(api_key_models) =
                    updates.get("api_key_models").and_then(|v| v.as_array())
                {
                    let incoming: Vec<ApiKeyModelConfig> = api_key_models
                        .iter()
                        .filter_map(|value| {
                            let api_key = value.get("api_key").and_then(|v| v.as_str())?;
                            let supported_models = value
                                .get("supported_models")
                                .and_then(|v| v.as_array())?
                                .iter()
                                .filter_map(|model| model.as_str().map(|s| s.to_string()))
                                .collect::<Vec<_>>();
                            Some(ApiKeyModelConfig {
                                api_key: api_key.to_string(),
                                supported_models,
                            })
                        })
                        .collect();
                    upstream.api_key_models =
                        merge_api_key_models(&upstream.api_key_models, &incoming);
                    upstream.supported_models = derive_supported_models(&upstream.api_key_models);
                }
                if updates.get("api_key_models").is_none() {
                    if let Some(supported_models) =
                        updates.get("supported_models").and_then(|v| v.as_array())
                    {
                        upstream.supported_models = supported_models
                            .iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect();
                    }
                }
                if let Some(protocols) = updates.get("protocols").and_then(Value::as_array) {
                    upstream.protocols = parse_upstream_protocols(protocols);
                } else if let Some(protocol) = updates.get("protocol").and_then(Value::as_str) {
                    upstream.protocol = parse_upstream_protocol(protocol);
                }
                if let Some(model_contexts) =
                    updates.get("model_contexts").and_then(|v| v.as_array())
                {
                    upstream.model_contexts = model_contexts
                        .iter()
                        .filter_map(|value| {
                            let slug = value.get("slug").and_then(|v| v.as_str())?;
                            let context_limit = value.get("context_limit").and_then(parse_u64_flexible)?;
                            let output_reserve = value
                                .get("output_reserve")
                                .and_then(parse_u64_flexible)
                                .unwrap_or(default_model_context_output_reserve() as u64);
                            let context_group = value
                                .get("context_group")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default();
                            Some(ModelContextConfig {
                                slug: slug.to_string(),
                                context_limit: context_limit as u32,
                                output_reserve: output_reserve as u32,
                                context_group: context_group.to_string(),
                            })
                        })
                        .collect();
                }
                if let Some(default_model_context_updates) = updates.get("default_model_context") {
                    if default_model_context_updates.is_null() {
                        upstream.default_model_context = None;
                    } else {
                        let context = {
                            let context_limit = default_model_context_updates
                                .get("context_limit")
                                .and_then(parse_u64_flexible);
                            let output_reserve = default_model_context_updates
                                .get("output_reserve")
                                .and_then(parse_u64_flexible)
                                .unwrap_or(default_model_context_output_reserve() as u64);
                            Some(DefaultModelContextConfig {
                                context_limit: context_limit.unwrap_or(0) as u32,
                                output_reserve: output_reserve as u32,
                                context_group: default_model_context_updates
                                    .get("context_group")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or_default()
                                    .to_string(),
                            })
                        };
                        upstream.default_model_context = context;
                    }
                }
                if let Some(request_quota_window_hours) = updates
                    .get("request_quota_window_hours")
                    .and_then(|v| v.as_u64())
                {
                    upstream.request_quota_window_hours = request_quota_window_hours as u32;
                }
                if let Some(request_quota_requests) =
                    updates.get("request_quota_requests").and_then(|v| v.as_u64())
                {
                    upstream.request_quota_requests = request_quota_requests as u32;
                }
                if let Some(request_quota_5h) =
                    updates.get("request_quota_5h").and_then(|v| v.as_u64())
                {
                    upstream.request_quota_requests = request_quota_5h as u32;
                }
                if let Some(requests_per_minute) =
                    updates.get("requests_per_minute").and_then(|v| v.as_u64())
                {
                    upstream.requests_per_minute = requests_per_minute as u32;
                }
                if let Some(max_concurrency) = updates.get("max_concurrency").and_then(|v| v.as_u64())
                {
                    upstream.max_concurrency = max_concurrency as u32;
                }
                if let Some(model_request_costs) =
                    updates.get("model_request_costs").and_then(|v| v.as_array())
                {
                    upstream.model_request_costs = model_request_costs
                        .iter()
                        .filter_map(|value| {
                            let slug = value.get("slug").and_then(|v| v.as_str())?;
                            let cost = value.get("cost").and_then(|v| v.as_f64())?;
                            Some(ModelRequestCostConfig {
                                slug: slug.to_string(),
                                cost,
                            })
                        })
                        .collect();
                }
                if let Some(priority) = updates.get("priority").and_then(|v| v.as_u64()) {
                    upstream.priority = priority as u32;
                }
                if let Some(premium_models) =
                    updates.get("premium_models").and_then(|v| v.as_array())
                {
                    upstream.premium_models = premium_models
                        .iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect();
                }
                if let Some(premium_only) = updates.get("premium_only").and_then(|v| v.as_bool()) {
                    upstream.premium_only = premium_only;
                }
                if let Some(protect_premium_quota) = updates
                    .get("protect_premium_quota")
                    .and_then(|v| v.as_bool())
                {
                    upstream.protect_premium_quota = protect_premium_quota;
                }
                if let Some(active) = updates.get("active").and_then(|v| v.as_bool()) {
                    upstream.active = active;
                }

                upstream.normalize_for_storage();
                if let Err(error) = upstream.validate_configuration() {
                    return Err(UpstreamMutationError::InvalidInput(error));
                }

                Ok(upstream.clone())
            },
            |e| UpstreamMutationError::Persist(format!("Failed to persist state: {e}")),
        )
        .await
    }
}
