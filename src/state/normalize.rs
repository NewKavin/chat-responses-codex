use super::types::*;
use crate::routing::UpstreamProtocol;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

pub(super) fn parse_upstream_protocol(value: &str) -> UpstreamProtocol {
    match value {
        "Responses" | "responses" => UpstreamProtocol::Responses,
        _ => UpstreamProtocol::ChatCompletions,
    }
}

pub(super) fn parse_upstream_protocols(values: &[Value]) -> Vec<UpstreamProtocol> {
    values
        .iter()
        .filter_map(Value::as_str)
        .map(parse_upstream_protocol)
        .collect()
}

pub(super) fn parse_u64_flexible(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        value
            .as_str()
            .and_then(|value| value.trim().parse::<u64>().ok())
    })
}

fn dedup_protocols(values: Vec<UpstreamProtocol>) -> Vec<UpstreamProtocol> {
    let mut normalized = Vec::new();
    for protocol in values {
        if !normalized.contains(&protocol) {
            normalized.push(protocol);
        }
    }
    normalized
}

fn normalized_string_list(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for value in values {
        let value = value.trim().to_string();
        if value.is_empty() || !seen.insert(value.clone()) {
            continue;
        }
        normalized.push(value);
    }
    normalized
}

fn normalized_current_keys(api_key: &str, api_keys: &[String]) -> Vec<String> {
    normalized_string_list(
        std::iter::once(api_key.to_string())
            .chain(api_keys.iter().cloned())
            .collect(),
    )
}

fn normalized_api_key_models(
    values: Vec<ApiKeyModelConfig>,
    current_keys: &[String],
) -> Vec<ApiKeyModelConfig> {
    let current_key_set = current_keys.iter().cloned().collect::<HashSet<_>>();
    let mut positions = HashMap::<String, usize>::new();
    let mut normalized: Vec<ApiKeyModelConfig> = Vec::new();
    for mut value in values {
        let api_key = value.api_key.trim().to_string();
        if api_key.is_empty() || !current_key_set.contains(&api_key) {
            continue;
        }
        let supported_models = normalized_string_list(std::mem::take(&mut value.supported_models));
        if let Some(index) = positions.get(&api_key).copied() {
            let mut merged = std::mem::take(&mut normalized[index].supported_models);
            merged.extend(supported_models);
            normalized[index].supported_models = normalized_string_list(merged);
        } else {
            positions.insert(api_key.clone(), normalized.len());
            normalized.push(ApiKeyModelConfig {
                api_key,
                supported_models,
            });
        }
    }

    for api_key in current_keys {
        if !positions.contains_key(api_key) {
            normalized.push(ApiKeyModelConfig {
                api_key: api_key.clone(),
                supported_models: Vec::new(),
            });
        }
    }
    normalized
}

fn derive_supported_models(key_models: &[ApiKeyModelConfig]) -> Vec<String> {
    normalized_string_list(
        key_models
            .iter()
            .flat_map(|mapping| mapping.supported_models.iter().cloned())
            .collect(),
    )
}
pub(super) fn normalized_model_contexts(
    values: Vec<ModelContextConfig>,
) -> Vec<ModelContextConfig> {
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for config in values {
        let slug = config.slug.trim().to_string();
        if slug.is_empty() || !seen.insert(slug.clone()) {
            continue;
        }
        let context_limit = config.context_limit.max(2);
        let mut output_reserve = if config.output_reserve == 0 {
            default_model_context_output_reserve()
        } else {
            config.output_reserve
        };
        output_reserve = output_reserve.min(context_limit.saturating_sub(1).max(1));
        normalized.push(ModelContextConfig {
            slug,
            context_limit,
            output_reserve,
            max_output_tokens: config.max_output_tokens,
            context_group: config.context_group.trim().to_string(),
        });
    }
    normalized
}

pub(super) fn normalized_default_model_context(
    value: Option<DefaultModelContextConfig>,
) -> Option<DefaultModelContextConfig> {
    let context = value?;
    if context.context_limit == 0 {
        return None;
    }

    let context_limit = context.context_limit.max(2);
    let mut output_reserve = if context.output_reserve == 0 {
        default_model_context_output_reserve()
    } else {
        context.output_reserve
    };
    output_reserve = output_reserve.min(context_limit.saturating_sub(1).max(1));

    Some(DefaultModelContextConfig {
        context_limit,
        output_reserve,
        max_output_tokens: context.max_output_tokens,
        context_group: context.context_group.trim().to_string(),
    })
}

impl UpstreamConfig {
    pub fn supported_protocols(&self) -> Vec<UpstreamProtocol> {
        let mut protocols = self.protocols.clone();
        if protocols.is_empty() {
            protocols.push(self.protocol);
        }
        dedup_protocols(protocols)
    }

    pub fn supports_protocol(&self, protocol: UpstreamProtocol) -> bool {
        self.supported_protocols().contains(&protocol)
    }

    pub fn route_models(&self) -> Vec<String> {
        let mut models = Vec::new();
        let mut seen = HashSet::new();

        for model in self
            .supported_models
            .iter()
            .chain(self.premium_models.iter())
        {
            let model = model.trim();
            if model.is_empty() {
                continue;
            }
            if seen.insert(model.to_string()) {
                models.push(model.to_string());
            }
        }

        models
    }

    pub fn supports_model(&self, model: &str) -> bool {
        self.supports_model_with(model, true)
    }

    pub fn supports_model_with(&self, model: &str, case_insensitive: bool) -> bool {
        self.canonical_route_model(model, case_insensitive).is_some()
    }

    /// Resolve the requested model to the upstream's *stored* spelling
    /// (default: case-insensitive canonical matching). The returned string is
    /// what the upstream sees on the wire and what
    /// `RouteHealthKey.runtime_model_slug` / the outbound payload use, so the
    /// stored casing is preserved exactly.
    pub fn resolved_model_name(&self, model: &str) -> Option<String> {
        self.resolved_model_name_with(model, true)
    }

    pub fn resolved_model_name_with(&self, model: &str, case_insensitive: bool) -> Option<String> {
        self.canonical_route_model(model, case_insensitive)
    }

    pub fn is_premium_model_request(&self, model: &str) -> bool {
        self.is_premium_model_request_with(model, true)
    }

    pub fn is_premium_model_request_with(&self, model: &str, case_insensitive: bool) -> bool {
        if self.premium_models.is_empty() {
            return false;
        }

        let model = model.trim();
        !model.is_empty()
            && self.premium_models.iter().any(|premium| {
                let premium = premium.trim();
                if case_insensitive {
                    super::models_equivalent_with(premium, model, true)
                        || super::codex_subagent_base_model(model).is_some_and(|base| {
                            super::models_equivalent_with(premium, base, true)
                        })
                } else {
                    premium == model
                        || super::codex_subagent_base_model(model).is_some_and(|base| premium == base)
                }
            })
    }
    pub fn request_quota_window_seconds(&self) -> u64 {
        u64::from(self.request_quota_window_hours.max(1)).saturating_mul(60 * 60)
    }

    pub fn premium_route_models(&self) -> Vec<String> {
        let mut models = Vec::new();
        let mut seen = HashSet::new();
        for premium in &self.premium_models {
            let premium = premium.trim();
            if premium.is_empty() {
                continue;
            }
            if seen.insert(premium.to_string()) {
                models.push(premium.to_string());
            }
        }
        models
    }

    pub fn normalize_for_storage(&mut self) {
        self.failure_count = 0;
        let authoritative_key_models = !self.api_key_models.is_empty();
        let api_key_models = std::mem::take(&mut self.api_key_models);
        let normalized_protocols = dedup_protocols(std::mem::take(&mut self.protocols));
        self.protocols = if normalized_protocols.is_empty() {
            vec![self.protocol]
        } else {
            normalized_protocols
        };
        self.protocol = self
            .protocols
            .first()
            .copied()
            .unwrap_or(UpstreamProtocol::ChatCompletions);
        self.remark = self.remark.trim().to_string();
        self.continuation_provider_group = self
            .continuation_provider_group
            .take()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        self.api_key = self.api_key.trim().to_string();
        self.api_keys = normalized_string_list(std::mem::take(&mut self.api_keys));
        let current_keys = normalized_current_keys(&self.api_key, &self.api_keys);
        if authoritative_key_models {
            self.api_key_models = normalized_api_key_models(api_key_models, &current_keys);
            self.supported_models = derive_supported_models(&self.api_key_models);
        } else {
            self.api_key_models.clear();
            self.supported_models =
                normalized_string_list(std::mem::take(&mut self.supported_models));
        }
        self.premium_models = normalized_string_list(std::mem::take(&mut self.premium_models));
        self.model_contexts = normalized_model_contexts(std::mem::take(&mut self.model_contexts));
        self.default_model_context =
            normalized_default_model_context(self.default_model_context.take());
    }

    pub fn validate_configuration(&self) -> Result<(), String> {
        if self.max_concurrency == 0 {
            return Err("max_concurrency must be greater than zero".to_string());
        }
        if self
            .continuation_provider_group
            .as_ref()
            .is_some_and(|value| value.len() > 128 || value.chars().any(char::is_control))
        {
            return Err(
                "continuation provider group must be 1-128 printable characters".to_string(),
            );
        }
        if self.premium_models.is_empty() {
            return Ok(());
        }

        let routable = self
            .supported_models
            .iter()
            .cloned()
            .collect::<HashSet<_>>();
        let unknown = self
            .premium_models
            .iter()
            .map(|model| model.trim().to_string())
            .filter(|model| !model.is_empty() && !routable.contains(model))
            .collect::<Vec<_>>();

        // Allow premium_models that are not yet in supported_models.
        // The upstream may be configured with premium models before model discovery,
        // or the premium model might match upstream route patterns.
        if !unknown.is_empty() {
            tracing::warn!(
                "premium_models contain models not yet in supported_models: {}",
                unknown.join(", ")
            );
        }

        Ok(())
    }

    fn canonical_route_model(&self, model: &str, case_insensitive: bool) -> Option<String> {
        let model = model.trim();
        if model.is_empty() {
            return None;
        }

        let route_models = self.route_models();
        if route_models.is_empty() {
            return super::codex_subagent_base_model(model)
                .is_none()
                .then(|| model.to_string());
        }

        if case_insensitive {
            // The first canonical match wins, including when a later entry is
            // an exact-case match. This keeps runtime spelling deterministic
            // when one upstream lists case-only duplicates.
            if let Some(candidate) = super::find_equivalent_stored(&route_models, model, true) {
                return Some(candidate.to_string());
            }
        } else if let Some(candidate) = super::find_equivalent_stored(&route_models, model, false) {
            return Some(candidate.to_string());
        }

        if let Some(base_model) = super::codex_subagent_base_model(model) {
            let base_matches = |candidate: &&String| {
                if case_insensitive {
                    super::models_equivalent_with(candidate, base_model, true)
                } else {
                    candidate == &base_model
                }
            };
            if let Some(candidate) = route_models.iter().find(base_matches) {
                return Some(candidate.clone());
            }
        }

        None
    }

    pub fn available_keys(&self) -> Vec<String> {
        normalized_current_keys(&self.api_key, &self.api_keys)
    }

    pub fn keys_for_model(&self, model: &str) -> Vec<String> {
        self.keys_for_model_with(model, true)
    }

    pub fn keys_for_model_with(&self, model: &str, case_insensitive: bool) -> Vec<String> {
        let model = model.trim();
        if self.api_key_models.is_empty() {
            return self.available_keys();
        }
        if model.is_empty() {
            return Vec::new();
        }

        let mut keys = Vec::new();
        let mut seen = HashSet::new();
        let current_keys = self.available_keys().into_iter().collect::<HashSet<_>>();
        for mapping in &self.api_key_models {
            let mapping_matches = if case_insensitive {
                mapping.supported_models.iter().any(|candidate| {
                    super::models_equivalent_with(candidate, model, true)
                        || super::codex_subagent_base_model(model).is_some_and(|base| {
                            super::models_equivalent_with(candidate, base, true)
                        })
                })
            } else {
                mapping
                    .supported_models
                    .iter()
                    .any(|candidate| candidate.trim() == model)
            };
            if !mapping_matches {
                continue;
            }

            let key = mapping.api_key.trim();
            if key.is_empty() || !current_keys.contains(key) {
                continue;
            }
            let key = key.to_string();
            if seen.insert(key.clone()) {
                keys.push(key);
            }
        }

        // When explicit model-to-key mappings are configured, a miss means this
        // upstream does not have a usable key for the requested model.
        keys
    }
}
