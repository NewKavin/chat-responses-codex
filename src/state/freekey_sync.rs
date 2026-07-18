use super::normalize::{parse_u64_flexible, parse_upstream_protocol, parse_upstream_protocols};
use super::types::*;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
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
    pub valid: bool,
}

pub(super) fn merge_api_keys(existing: &[String], incoming: &[String]) -> Vec<String> {
    let mut keys: Vec<String> = existing.to_vec();
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
                    valid: item.valid,
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

        let (result, touched_upstream_ids) = self
            .mutate_persisted_state(
                |candidate_state| {
                    let mut result = FreekeySyncSummary::default();
                    let mut touched_upstream_ids = HashSet::new();

                    let find_auto_upstream = |state: &[UpstreamConfig], base_url: &str| {
                        state.iter().position(|upstream| {
                            upstream.base_url == base_url && upstream.auto_managed
                        })
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
                            // Only valid items contribute keys/models. Invalid
                            // items are carried so the group still reaches the
                            // replace branch (enabling key-set clearing) but do
                            // not add keys back.
                            if !item.valid {
                                if let Some(name) = &item.name {
                                    last_name = Some(name.clone());
                                }
                                continue;
                            }
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
                        // Creating a brand-new upstream requires at least one valid
                        // key+model; an all-invalid payload only makes sense for an
                        // existing upstream (clearing its keys), so skip creation.
                        if keys.is_empty() || models.is_empty() {
                            if let Some(index) =
                                find_auto_upstream(&candidate_state.upstreams, base_url)
                            {
                                let upstream = &mut candidate_state.upstreams[index];
                                upstream.api_keys = Vec::new();
                                upstream.api_key = String::new();
                                upstream.api_key_models = Vec::new();
                                upstream.supported_models = Vec::new();
                                if let Some(name) = last_name {
                                    upstream.name = name;
                                }
                                upstream.managed_source = Some(source.clone());
                                upstream.last_synced_at = synced_at;
                                upstream.normalize_for_storage();
                                touched_upstream_ids.insert(upstream.id.clone());
                                result.updated = result.updated.saturating_add(items.len());
                            } else {
                                result.skipped = result.skipped.saturating_add(items.len());
                            }
                            continue;
                        }

                        if let Some(index) =
                            find_auto_upstream(&candidate_state.upstreams, base_url)
                        {
                            let upstream = &mut candidate_state.upstreams[index];
                            // Replace semantics: the submitted valid key set is the
                            // new source of truth. Keys absent from the payload are
                            // removed (the external script is responsible for
                            // probing and reports only still-valid keys).
                            upstream.api_keys = keys.clone();
                            // Sync the legacy single-key field to the first key so
                            // it cannot resurrect deleted keys via the routing
                            // fallback. Empty key set clears it.
                            upstream.api_key = keys.first().cloned().unwrap_or_default();
                            // Replace api_key_models with the incoming mappings,
                            // preserving existing supported_models for keys that
                            // survive the replace (the backend does not probe here;
                            // model mappings are refreshed via discover-models).
                            let surviving_keys: HashSet<String> = keys.iter().cloned().collect();
                            let preserved: HashMap<String, Vec<String>> = upstream
                                .api_key_models
                                .iter()
                                .filter(|entry| surviving_keys.contains(&entry.api_key))
                                .map(|entry| {
                                    (entry.api_key.clone(), entry.supported_models.clone())
                                })
                                .collect();
                            upstream.api_key_models = key_models
                                .iter()
                                .map(|entry| {
                                    let preserved_models = preserved.get(&entry.api_key).cloned();
                                    ApiKeyModelConfig {
                                        api_key: entry.api_key.clone(),
                                        supported_models: if entry.supported_models.is_empty() {
                                            preserved_models.unwrap_or_default()
                                        } else {
                                            entry.supported_models.clone()
                                        },
                                    }
                                })
                                .filter(|entry: &ApiKeyModelConfig| {
                                    !entry.supported_models.is_empty()
                                })
                                .collect();
                            // Re-attach any surviving key whose models we preserved
                            // but which had no incoming mapping.
                            let mapped_keys: HashSet<String> = upstream
                                .api_key_models
                                .iter()
                                .map(|e| e.api_key.clone())
                                .collect();
                            for (key, models) in &preserved {
                                if !mapped_keys.contains(key) && !models.is_empty() {
                                    upstream.api_key_models.push(ApiKeyModelConfig {
                                        api_key: key.clone(),
                                        supported_models: models.clone(),
                                    });
                                }
                            }
                            upstream.supported_models =
                                derive_supported_models(&upstream.api_key_models);
                            if let Some(name) = last_name {
                                upstream.name = name;
                            }
                            upstream.managed_source = Some(source.clone());
                            upstream.last_synced_at = synced_at;
                            upstream.normalize_for_storage();
                            touched_upstream_ids.insert(upstream.id.clone());

                            result.updated = result
                                .updated
                                .saturating_add(items.iter().filter(|i| i.valid).count());
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
                        touched_upstream_ids.insert(upstream.id.clone());
                        candidate_state.upstreams.push(upstream);
                        result.created = result
                            .created
                            .saturating_add(items.iter().filter(|i| i.valid).count());
                    }

                    Ok((result, touched_upstream_ids))
                },
                |error| format!("Failed to persist state: {}", error),
            )
            .await?;

        let routing = self.routing_snapshot().await;
        let jobs = self.stale_capability_probe_jobs_for_upstreams(
            routing
                .upstreams
                .iter()
                .filter(|upstream| touched_upstream_ids.contains(&upstream.id)),
            synced_at,
        );
        self.submit_capability_probe_jobs(
            jobs,
            crate::capabilities::ProbeReason::ConfigurationChanged,
        )
        .await
        .map_err(|error| error.to_string())?;

        Ok(result)
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
                // Check for replace mode flag from admin_update_upstream key validation.
                let replace_api_keys =
                    updates.get("_replace_api_keys").and_then(|v| v.as_bool()) == Some(true);
                let replace_api_key_models = replace_api_keys
                    && updates
                        .get("api_key_models")
                        .and_then(|v| v.as_array())
                        .is_some();

                if replace_api_keys {
                    let explicit = updates
                        .get("api_key")
                        .and_then(|v| v.as_str())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty());
                    let replacement_key_set: HashSet<String> = explicit
                        .iter()
                        .cloned()
                        .chain(
                            updates
                                .get("api_keys")
                                .and_then(|v| v.as_array())
                                .into_iter()
                                .flatten()
                                .filter_map(|v| v.as_str())
                                .map(|s| s.trim().to_string())
                                .filter(|s| !s.is_empty()),
                        )
                        .collect();

                    // Directly replace api_keys and api_key_models with validated keys.
                    if let Some(api_keys) = updates.get("api_keys").and_then(|v| v.as_array()) {
                        upstream.api_keys = api_keys
                            .iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect();
                        // Sync the legacy single-key field so it cannot
                        // resurrect deleted keys via the routing fallback.
                        // Prefer an explicit api_key from the payload, else the
                        // first of the replacement set.
                        upstream.api_key = explicit
                            .or_else(|| upstream.api_keys.first().cloned())
                            .unwrap_or_default();
                    }
                    if let Some(api_key_models) =
                        updates.get("api_key_models").and_then(|v| v.as_array())
                    {
                        upstream.api_key_models = api_key_models
                            .iter()
                            .filter_map(|value| {
                                let api_key = value.get("api_key").and_then(|v| v.as_str())?.trim();
                                let supported_models = value
                                    .get("supported_models")
                                    .and_then(|v| v.as_array())?
                                    .iter()
                                    .filter_map(|model| model.as_str().map(|s| s.to_string()))
                                    .collect::<Vec<_>>();
                                if !replacement_key_set.contains(api_key) {
                                    return None;
                                }
                                Some(ApiKeyModelConfig {
                                    api_key: api_key.to_string(),
                                    supported_models,
                                })
                            })
                            .collect();
                    }
                } else {
                    // Legacy merge behavior (only adds, never removes).
                    if let Some(api_key) = updates.get("api_key").and_then(|v| v.as_str()) {
                        let new_key = api_key.to_string();
                        if !new_key.is_empty() {
                            let mut merged =
                                merge_api_keys(&upstream.api_keys, std::slice::from_ref(&new_key));
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
                }
                if !replace_api_keys {
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
                        upstream.supported_models =
                            derive_supported_models(&upstream.api_key_models);
                    }
                }
                if replace_api_keys {
                    if replace_api_key_models || !upstream.api_key_models.is_empty() {
                        upstream.supported_models =
                            derive_supported_models(&upstream.api_key_models);
                    } else if let Some(supported_models) =
                        updates.get("supported_models").and_then(|v| v.as_array())
                    {
                        upstream.supported_models = supported_models
                            .iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect();
                    }
                } else if updates.get("api_key_models").is_none() {
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
                            let context_limit =
                                value.get("context_limit").and_then(parse_u64_flexible)?;
                            let output_reserve = value
                                .get("output_reserve")
                                .and_then(parse_u64_flexible)
                                .unwrap_or(default_model_context_output_reserve() as u64);
                            let context_group = value
                                .get("context_group")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default();
                            let max_output_tokens = value
                                .get("max_output_tokens")
                                .and_then(parse_u64_flexible)
                                .unwrap_or(0)
                                as u32;
                            Some(ModelContextConfig {
                                slug: slug.to_string(),
                                context_limit: context_limit as u32,
                                output_reserve: output_reserve as u32,
                                max_output_tokens,
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
                            let max_output_tokens = default_model_context_updates
                                .get("max_output_tokens")
                                .and_then(parse_u64_flexible)
                                .unwrap_or(0)
                                as u32;
                            Some(DefaultModelContextConfig {
                                context_limit: context_limit.unwrap_or(0) as u32,
                                output_reserve: output_reserve as u32,
                                max_output_tokens,
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
                if let Some(request_quota_requests) = updates
                    .get("request_quota_requests")
                    .and_then(|v| v.as_u64())
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
                if let Some(max_concurrency) =
                    updates.get("max_concurrency").and_then(|v| v.as_u64())
                {
                    upstream.max_concurrency = max_concurrency as u32;
                }
                if let Some(model_request_costs) = updates
                    .get("model_request_costs")
                    .and_then(|v| v.as_array())
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
                if let Some(strip_nonstandard_chat_fields) = updates
                    .get("strip_nonstandard_chat_fields")
                    .and_then(|v| v.as_bool())
                {
                    upstream.strip_nonstandard_chat_fields = strip_nonstandard_chat_fields;
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
