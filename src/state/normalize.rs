use super::types::*;
use crate::routing::UpstreamProtocol;
use serde_json::Value;
use std::collections::HashSet;

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

fn normalized_api_key_models(values: Vec<ApiKeyModelConfig>) -> Vec<ApiKeyModelConfig> {
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for mut value in values {
        let api_key = value.api_key.trim().to_string();
        if api_key.is_empty() || !seen.insert(api_key.clone()) {
            continue;
        }
        let supported_models = normalized_string_list(std::mem::take(&mut value.supported_models));
        if supported_models.is_empty() {
            continue;
        }
        normalized.push(ApiKeyModelConfig {
            api_key,
            supported_models,
        });
    }
    normalized
}

fn normalized_model_request_costs(
    values: Vec<ModelRequestCostConfig>,
) -> Vec<ModelRequestCostConfig> {
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for rule in values {
        let slug = rule.slug.trim().to_string();
        if slug.is_empty() || !seen.insert(slug.clone()) {
            continue;
        }
        normalized.push(ModelRequestCostConfig {
            slug,
            cost: rule.cost,
        });
    }
    normalized
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
            context_group: config.context_group.trim().to_string(),
        });
    }
    normalized
}

pub(super) fn normalized_default_model_context(
    value: Option<DefaultModelContextConfig>,
) -> Option<DefaultModelContextConfig> {
    let Some(context) = value else {
        return None;
    };
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
        self.canonical_route_model(model).is_some()
    }

    pub fn resolved_model_name(&self, model: &str) -> Option<String> {
        self.canonical_route_model(model)
    }

    pub fn is_premium_model_request(&self, model: &str) -> bool {
        if self.premium_models.is_empty() {
            return false;
        }

        let model = model.trim();
        !model.is_empty()
            && self
                .premium_models
                .iter()
                .any(|premium| premium.trim() == model)
    }

    pub fn request_cost_for_model(&self, model: &str) -> f64 {
        let model = model.trim();
        if model.is_empty() {
            return 1.0;
        }

        self.model_request_costs
            .iter()
            .find(|rule| rule.slug.trim() == model)
            .map(|rule| rule.cost.max(1.0))
            .unwrap_or(1.0)
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
        self.api_keys = normalized_string_list(std::mem::take(&mut self.api_keys));
        self.api_key_models = normalized_api_key_models(std::mem::take(&mut self.api_key_models));
        self.supported_models = normalized_string_list(std::mem::take(&mut self.supported_models));
        if !self.api_key_models.is_empty() {
            let mut seen = self
                .supported_models
                .iter()
                .cloned()
                .collect::<HashSet<_>>();
            for mapping in &self.api_key_models {
                for model in &mapping.supported_models {
                    if seen.insert(model.clone()) {
                        self.supported_models.push(model.clone());
                    }
                }
            }
        }
        self.premium_models = normalized_string_list(std::mem::take(&mut self.premium_models));
        self.model_request_costs =
            normalized_model_request_costs(std::mem::take(&mut self.model_request_costs));
        self.model_contexts = normalized_model_contexts(std::mem::take(&mut self.model_contexts));
        self.default_model_context =
            normalized_default_model_context(self.default_model_context.take());
    }

    pub fn validate_configuration(&self) -> Result<(), String> {
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

    fn canonical_route_model(&self, model: &str) -> Option<String> {
        let model = model.trim();
        if model.is_empty() {
            return None;
        }

        let route_models = self.route_models();
        if route_models.is_empty() {
            return Some(model.to_string());
        }

        if route_models.iter().any(|candidate| candidate == model) {
            return Some(model.to_string());
        }

        None
    }

    pub fn available_keys(&self) -> Vec<String> {
        let mut keys = Vec::new();
        let mut seen = HashSet::new();
        for key in self
            .api_keys
            .iter()
            .chain(std::iter::once(&self.api_key))
            .chain(self.api_key_models.iter().map(|item| &item.api_key))
        {
            let key = key.trim();
            if key.is_empty() {
                continue;
            }
            let key = key.to_string();
            if seen.insert(key.clone()) {
                keys.push(key);
            }
        }
        keys
    }

    pub fn keys_for_model(&self, model: &str) -> Vec<String> {
        let model = model.trim();
        if model.is_empty() || self.api_key_models.is_empty() {
            return self.available_keys();
        }

        let mut keys = Vec::new();
        let mut seen = HashSet::new();
        for mapping in &self.api_key_models {
            if !mapping
                .supported_models
                .iter()
                .any(|candidate| candidate.trim() == model)
            {
                continue;
            }

            let key = mapping.api_key.trim();
            if key.is_empty() {
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
