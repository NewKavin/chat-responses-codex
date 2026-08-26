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
        self.canonical_route_model(model, case_insensitive)
            .is_some()
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

    /// T3.4: resolve the dialect preset for a specific runtime model slug.
    /// Priority per plan: a matching `model_dialect_presets` entry (exact
    /// canonical match first, then `prefix*` wildcard) wins over the
    /// per-upstream `dialect_preset` fallback. A verified probe profile beats
    /// both at the resolver (it is consulted only when no profile exists).
    /// Matching syntax reuses the model-mappings convention (canonical
    /// trimmed+lowercase comparison) plus an optional trailing `*` prefix
    /// wildcard, e.g. `{"glm-*": "glm", "deepseek-*": "deepseek"}`.
    pub fn dialect_preset_for_model(&self, runtime_model_slug: &str) -> Option<&str> {
        if !self.model_dialect_presets.is_empty() {
            let canonical = super::canonical_model_id(runtime_model_slug);
            // 1) Exact canonical match wins first.
            if let Some(preset) = self
                .model_dialect_presets
                .iter()
                .find(|(pattern, _)| super::canonical_model_id(pattern.trim()) == canonical)
            {
                return Some(preset.1);
            }
            // 2) Longest matching `prefix*` wildcard wins next; ties broken by
            //    BTreeMap key order (stable).
            if let Some((_, preset)) = self
                .model_dialect_presets
                .iter()
                .filter(|(pattern, _)| pattern.trim_end().ends_with('*'))
                .filter_map(|(pattern, preset)| {
                    let prefix = super::canonical_model_id(pattern.trim_end_matches('*'));
                    (!prefix.is_empty() && canonical.starts_with(&prefix))
                        .then_some((prefix.len(), preset))
                })
                .max_by_key(|(prefix_len, _)| *prefix_len)
            {
                return Some(preset);
            }
        }
        self.dialect_preset.as_deref()
    }

    pub fn is_premium_model_request_with(&self, model: &str, case_insensitive: bool) -> bool {
        if self.premium_models.is_empty() {
            return false;
        }

        let model = model.trim();
        if model.is_empty() {
            return false;
        }

        let matches = |candidate: &str| {
            self.premium_models.iter().any(|premium| {
                let premium = premium.trim();
                if case_insensitive {
                    super::models_equivalent_with(premium, candidate, true)
                        || super::codex_subagent_base_model(candidate)
                            .is_some_and(|base| super::models_equivalent_with(premium, base, true))
                } else {
                    premium == candidate
                        || super::codex_subagent_base_model(candidate)
                            .is_some_and(|base| premium == base)
                }
            })
        };

        // Per-upstream model mappings rename the downstream view: premium is
        // judged against the upstream's original spelling (3.3.3). Upstream A
        // with premium_models ["gpt-4"] and mapping gpt-4 -> gpt-4-premium
        // counts requests for "gpt-4-premium" as premium requests.
        if let Some(resolved) = self.canonical_route_model(model, case_insensitive) {
            if matches(&resolved) {
                return true;
            }
        }
        // Legacy fallback: upstreams that cannot resolve this request keep the
        // raw-name comparison (quota-protection decisions are per-upstream).
        matches(model)
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
        self.model_mappings = std::mem::take(&mut self.model_mappings)
            .into_iter()
            .map(|mapping| UpstreamModelMapping {
                upstream_model: mapping.upstream_model.trim().to_string(),
                downstream_model: mapping.downstream_model.trim().to_string(),
            })
            .collect();
        self.model_contexts = normalized_model_contexts(std::mem::take(&mut self.model_contexts));
        self.default_model_context =
            normalized_default_model_context(self.default_model_context.take());
        // T3.4: trim keys and values; drop empty entries so a stale `""`
        // mapping never shadows anything.
        self.model_dialect_presets = std::mem::take(&mut self.model_dialect_presets)
            .into_iter()
            .map(|(pattern, preset)| (pattern.trim().to_string(), preset.trim().to_string()))
            .filter(|(pattern, preset)| !pattern.is_empty() && !preset.is_empty())
            .collect();
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
        self.validate_model_mappings()?;
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

    /// Validate per-upstream model mappings (Part B-3, rules 1-4 + 6 from the
    /// plan): non-empty names, per-upstream unique upstream/downstream names
    /// (canonical comparison), and no collision between a downstream name and
    /// an unmapped route model of the same upstream. Rule 5 (global alias
    /// collision) needs the alias registry and lives in
    /// [`Self::validate_model_mappings_against_aliases`].
    pub fn validate_model_mappings(&self) -> Result<(), String> {
        let mappings = self
            .model_mappings
            .iter()
            .filter_map(|mapping| {
                let upstream = mapping.upstream_model.trim();
                let downstream = mapping.downstream_model.trim();
                (!upstream.is_empty() || !downstream.is_empty())
                    .then(|| (upstream.to_string(), downstream.to_string()))
            })
            .collect::<Vec<_>>();
        for (index, (upstream, downstream)) in mappings.iter().enumerate() {
            if upstream.is_empty() {
                return Err(format!(
                    "model mapping at index {index} has an empty upstream_model"
                ));
            }
            if downstream.is_empty() {
                return Err(format!(
                    "model mapping at index {index} for upstream model '{upstream}' has an empty downstream_model"
                ));
            }
            for (other_index, (other_upstream, other_downstream)) in mappings.iter().enumerate() {
                if index == other_index {
                    continue;
                }
                if super::models_equivalent(upstream, other_upstream) {
                    return Err(format!(
                        "upstream model '{upstream}' is mapped more than once (conflicts with index {other_index} mapping)"
                    ));
                }
                if super::models_equivalent(downstream, other_downstream) {
                    return Err(format!(
                        "downstream model '{downstream}' is used by more than one mapping (conflicts with index {other_index}); each downstream name must map to exactly one upstream model"
                    ));
                }
            }
        }
        // A mapped downstream name must not collide (canonical) with a
        // route model that is not itself occupied by a mapping; otherwise the
        // same name has two sources on this upstream.
        let occupied_models = mappings
            .iter()
            .map(|(upstream, _)| super::canonical_model_id(upstream))
            .collect::<HashSet<_>>();
        for (index, (_, downstream)) in mappings.iter().enumerate() {
            let downstream_key = super::canonical_model_id(downstream);
            if let Some(collision) = self.route_models().iter().find(|route_model| {
                !occupied_models.contains(&super::canonical_model_id(route_model))
                    && super::canonical_model_id(route_model) == downstream_key
            }) {
                return Err(format!(
                    "downstream model '{downstream}' (mapping index {index}) collides with this upstream's own model '{collision}'; pick a different downstream name or map '{collision}' too"
                ));
            }
        }
        Ok(())
    }

    /// Rule 5: a mapped downstream name must not be an alias in any global
    /// model alias rule (the entry normalization would rewrite it before it
    /// can match the mapping).
    pub fn validate_model_mappings_against_aliases(
        &self,
        alias_registry: &super::model_identity::ModelAliasRegistry,
    ) -> Result<(), String> {
        for mapping in &self.model_mappings {
            let downstream = mapping.downstream_model.trim();
            if downstream.is_empty() {
                continue;
            }
            if let Some(canonical) = alias_registry.resolve_alias(downstream) {
                return Err(format!(
                    "model mapping downstream name '{downstream}' is an alias for '{canonical}' in a global rule; use '{canonical}' or remove the global rule"
                ));
            }
        }
        Ok(())
    }

    fn canonical_route_model(&self, model: &str, case_insensitive: bool) -> Option<String> {
        let model = model.trim();
        if model.is_empty() {
            return None;
        }

        let route_models = self.route_models();

        // 3.3.1: per-upstream model mappings win over case folding. The
        // mapping key is the downstream-facing name; the resolved value is the
        // upstream's *stored* spelling, which every upstream-side check
        // (payload model, RouteHealthKey.runtime_model_slug, premium,
        // ModelContextConfig.slug, keys) then uses.
        for mapping in &self.model_mappings {
            let downstream = mapping.downstream_model.trim();
            let matches = if case_insensitive {
                super::models_equivalent_with(downstream, model, true)
            } else {
                downstream == model
            };
            if !matches {
                continue;
            }
            let upstream = mapping.upstream_model.trim();
            if upstream.is_empty() {
                continue;
            }
            if let Some(stored) =
                super::find_equivalent_stored(&route_models, upstream, case_insensitive)
            {
                return Some(stored.to_string());
            }
            // Stale mapping: the upstream_model is not in any current model
            // list (model sync may have removed it). Skip the mapping; the
            // fallback below still runs, and it revives automatically once
            // the model is restored without any config change.
            tracing::warn!(
                upstream_id = %self.id,
                upstream_model = %upstream,
                downstream_model = %downstream,
                "stale model mapping skipped: upstream_model is not in the upstream's model lists (removed-model)"
            );
            continue;
        }

        // 3.3.2: fall back to canonical case folding, but never resolve a
        // model name that a mapping has claimed as the upstream side (rename
        // semantics: the original spelling is shadowed downstream).
        let occupied = self
            .model_mappings
            .iter()
            .map(|mapping| super::canonical_model_id(mapping.upstream_model.trim()))
            .collect::<HashSet<_>>();
        let unoccupied =
            |candidate: &&String| !occupied.contains(&super::canonical_model_id(candidate));

        if route_models.is_empty() {
            return super::codex_subagent_base_model(model)
                .is_none()
                .then(|| model.to_string());
        }

        if let Some(candidate) = route_models.iter().filter(unoccupied).find(|candidate| {
            if case_insensitive {
                super::models_equivalent_with(candidate, model, true)
            } else {
                candidate.trim() == model
            }
        }) {
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
            if let Some(candidate) = route_models.iter().filter(unoccupied).find(base_matches) {
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

    /// Upstream-space spelling check: is `model` currently a routable *stored*
    /// spelling of this upstream, ignoring per-upstream model mappings?
    /// Mappings rename the downstream view; the stored spelling itself stays
    /// routable upstream-side. Used by profile learning/GC paths that receive
    /// runtime slugs (which are upstream spellings by construction).
    pub fn supports_stored_model(&self, model: &str) -> bool {
        let model = model.trim();
        !model.is_empty()
            && self
                .route_models()
                .iter()
                .any(|stored| super::models_equivalent_with(stored, model, true))
    }

    /// Downstream-facing effective model set for this upstream (plan 3.4):
    /// every live mapping's downstream name followed by every route model
    /// that no mapping claims as its upstream side — the mapped originals
    /// are shadowed downstream (rename semantics) unless another upstream
    /// happens to declare them unmapped. A mapping whose `upstream_model` is
    /// no longer in any model list is stale: it contributes nothing (and its
    /// downstream name disappears from lists) until the model is restored.
    /// Stored spelling is preserved; callers keep applying the global alias
    /// display + canonical dedup pipeline on top of this set, exactly like
    /// they do for `route_models()` today.
    pub fn effective_downstream_models(&self) -> Vec<String> {
        self.effective_downstream_models_detailed()
            .into_iter()
            .map(|entry| entry.model)
            .collect()
    }

    /// Like [`Self::effective_downstream_models`] but keeps per-entry
    /// provenance: `from_mapping` marks names that come from a per-upstream
    /// model mapping (admin-picked downstream spelling). Mapping names are
    /// displayed verbatim — they are explicit operator labels and must never
    /// be case-folded — while unmapped route models keep the existing
    /// alias/canonical display pipeline.
    pub fn effective_downstream_models_detailed(&self) -> Vec<DownstreamModelEntry> {
        let route_models = self.route_models();
        let mut models = Vec::new();
        for mapping in &self.model_mappings {
            let upstream = mapping.upstream_model.trim();
            let downstream = mapping.downstream_model.trim();
            if upstream.is_empty() || downstream.is_empty() {
                continue;
            }
            if super::find_equivalent_stored(&route_models, upstream, true).is_none() {
                // Stale mapping (model sync may have removed the model):
                // skip until the stored spelling is restored.
                continue;
            }
            models.push(DownstreamModelEntry {
                model: downstream.to_string(),
                from_mapping: true,
            });
        }
        let occupied = self
            .model_mappings
            .iter()
            .map(|mapping| super::canonical_model_id(mapping.upstream_model.trim()))
            .collect::<HashSet<_>>();
        for candidate in &route_models {
            if occupied.contains(&super::canonical_model_id(candidate)) {
                continue;
            }
            models.push(DownstreamModelEntry {
                model: candidate.clone(),
                from_mapping: false,
            });
        }
        models
    }
}

/// A single downstream-facing model name surfaced by an upstream, with
/// provenance for display decisions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownstreamModelEntry {
    /// The downstream-facing model name (stored spelling; never rewritten).
    pub model: String,
    /// `true` when this name is an admin-picked per-upstream mapping label
    /// and must be displayed verbatim; `false` for unmapped route models
    /// (which keep the canonical/alias display pipeline).
    pub from_mapping: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::model_identity::{ModelAliasRegistry, ModelAliasRule};
    use serde_json::json;

    fn mapping(upstream_model: &str, downstream_model: &str) -> UpstreamModelMapping {
        UpstreamModelMapping {
            upstream_model: upstream_model.to_string(),
            downstream_model: downstream_model.to_string(),
        }
    }

    fn mapped_upstream() -> UpstreamConfig {
        UpstreamConfig {
            name: "mapped".into(),
            base_url: "https://example.invalid".into(),
            api_key: "key-a".into(),
            supported_models: vec!["gpt-4".into(), "gpt-4o".into()],
            model_mappings: vec![mapping("gpt-4", "gpt-4-premium")],
            ..UpstreamConfig::default()
        }
    }

    #[test]
    fn model_mappings_missing_old_config_field_defaults_to_empty() {
        let upstream: UpstreamConfig = serde_json::from_value(json!({
            "id": "up-old",
            "name": "legacy",
            "base_url": "https://example.invalid",
            "api_key": "key-a",
            "protocol": "ChatCompletions",
            "protocols": ["ChatCompletions"],
            "supported_models": ["gpt-4"],
            "active": true
        }))
        .unwrap();
        assert!(upstream.model_mappings.is_empty());
    }

    #[test]
    fn model_mappings_roundtrip_through_json() {
        let upstream = mapped_upstream();
        let decoded: UpstreamConfig =
            serde_json::from_value(serde_json::to_value(&upstream).unwrap()).unwrap();
        assert_eq!(decoded, upstream);
    }

    #[test]
    fn model_mappings_validate_valid_configuration() {
        let upstream = mapped_upstream();
        assert!(upstream.validate_configuration().is_ok());
        assert!(upstream
            .validate_model_mappings_against_aliases(&ModelAliasRegistry::default())
            .is_ok());
    }

    #[test]
    fn model_mappings_reject_empty_upstream_or_downstream() {
        let upstream = UpstreamConfig {
            model_mappings: vec![mapping("", "gpt-4-premium")],
            ..mapped_upstream()
        };
        let err = upstream.validate_configuration().unwrap_err();
        assert!(err.contains("upstream_model"), "message was: {err}");

        let upstream = UpstreamConfig {
            model_mappings: vec![mapping("gpt-4", "  ")],
            ..mapped_upstream()
        };
        let err = upstream.validate_configuration().unwrap_err();
        assert!(err.contains("downstream_model"), "message was: {err}");
    }

    #[test]
    fn model_mappings_reject_duplicate_upstream_model_canonically() {
        let upstream = UpstreamConfig {
            supported_models: vec!["gpt-4".into(), "gpt-4o".into()],
            model_mappings: vec![
                mapping("gpt-4", "gpt-4-premium"),
                mapping("GPT-4", "gpt-4-std"),
            ],
            ..UpstreamConfig::default()
        };
        let err = upstream.validate_configuration().unwrap_err();
        assert!(err.contains("gpt-4"), "message was: {err}");
    }

    #[test]
    fn model_mappings_reject_duplicate_downstream_model_canonically() {
        let upstream = UpstreamConfig {
            model_mappings: vec![
                mapping("gpt-4", "gpt-4-premium"),
                mapping("gpt-4o", "GPT-4-PREMIUM"),
            ],
            ..mapped_upstream()
        };
        let err = upstream.validate_configuration().unwrap_err();
        assert!(err.contains("gpt-4-premium"), "message was: {err}");
    }

    #[test]
    fn model_mappings_reject_downstream_colliding_with_unmapped_model() {
        // downstream "gpt-4o" collides with the unmapped route model "gpt-4o".
        let upstream = UpstreamConfig {
            model_mappings: vec![mapping("gpt-4", "gpt-4o")],
            ..mapped_upstream()
        };
        let err = upstream.validate_configuration().unwrap_err();
        assert!(err.contains("gpt-4o"), "message was: {err}");
    }

    #[test]
    fn model_mappings_premium_judged_on_upstream_spelling() {
        let mut upstream = mapped_upstream();
        upstream.premium_models = vec!["gpt-4".into()];
        upstream.protect_premium_quota = true;
        // Mapped request: premium must be judged against the upstream's
        // original spelling (A: premium_models ["gpt-4"], mapping
        // gpt-4 -> gpt-4-premium; request "gpt-4-premium" counts premium).
        assert!(upstream.is_premium_model_request("gpt-4-premium"));
        // An unrelated unmapped model is not premium.
        assert!(!upstream.is_premium_model_request("gpt-4o"));
        // The shadowed original spelling is not routable, but the legacy
        // raw-name check stays monotonic for quota-protection decisions.
        assert!(upstream.is_premium_model_request("gpt-4"));
    }

    #[test]
    fn model_mappings_allow_stale_upstream_model_but_reject_alias_downstream() {
        // upstream_model not in any model list: allowed (validation passes).
        let stale = UpstreamConfig {
            model_mappings: vec![mapping("removed-model", "gpt-4-premium")],
            ..mapped_upstream()
        };
        assert!(stale.validate_configuration().is_ok());

        // downstream hitting a global alias would be re-normalized away.
        let registry = ModelAliasRegistry::from_rules(vec![ModelAliasRule {
            canonical: "deepseek-v3".into(),
            aliases: vec!["deepseek-chat".into()],
        }])
        .unwrap();
        let conflicting = UpstreamConfig {
            model_mappings: vec![mapping("gpt-4", "deepseek-chat")],
            ..mapped_upstream()
        };
        let err = conflicting
            .validate_model_mappings_against_aliases(&registry)
            .unwrap_err();
        assert!(err.contains("deepseek-chat"), "message was: {err}");
        // Mapping to the rule's canonical is fine.
        let ok = UpstreamConfig {
            model_mappings: vec![mapping("gpt-4", "deepseek-v3")],
            ..mapped_upstream()
        };
        assert!(ok
            .validate_model_mappings_against_aliases(&registry)
            .is_ok());
    }
    #[test]
    fn model_mappings_effective_downstream_models_maps_names_then_unmapped_spellings() {
        // gpt-4 -> gpt-4-premium mapping, gpt-4o unmapped.
        assert_eq!(
            mapped_upstream().effective_downstream_models(),
            vec!["gpt-4-premium".to_string(), "gpt-4o".to_string()]
        );
    }

    #[test]
    fn model_mappings_effective_downstream_models_shadows_mapped_originals() {
        let models = mapped_upstream().effective_downstream_models();
        assert!(
            !models.iter().any(|model| model == "gpt-4"),
            "the mapped original spelling must be shadowed downstream: {models:?}"
        );
    }

    #[test]
    fn model_mappings_effective_downstream_models_skips_stale_mappings() {
        // upstream_model no longer in any model list: its downstream name
        // must not be exposed until the model is restored.
        let mut upstream = mapped_upstream();
        upstream
            .model_mappings
            .push(mapping("removed-model", "gpt-x"));
        assert_eq!(
            upstream.effective_downstream_models(),
            vec!["gpt-4-premium".to_string(), "gpt-4o".to_string()]
        );
    }

    #[test]
    fn model_mappings_effective_downstream_models_preserves_unmapped_stored_spelling() {
        let mut upstream = mapped_upstream();
        upstream.supported_models = vec!["gpt-4".into(), "DeepSeek-Chat".into()];
        upstream.model_mappings = vec![mapping("gpt-4", "gpt-4-premium")];
        assert_eq!(
            upstream.effective_downstream_models(),
            vec!["gpt-4-premium".to_string(), "DeepSeek-Chat".to_string()]
        );
    }

    #[test]
    fn model_mappings_effective_downstream_models_matches_upstream_side_canonically() {
        // The mapping's upstream spelling only needs to canonically match a
        // stored model (the wire payload keeps the stored spelling).
        let mut upstream = mapped_upstream();
        upstream.model_mappings = vec![mapping("GPT-4", "gpt-4-premium")];
        assert_eq!(
            upstream.effective_downstream_models(),
            vec!["gpt-4-premium".to_string(), "gpt-4o".to_string()]
        );
    }

    #[test]
    fn model_mappings_effective_downstream_models_defaults_to_route_models() {
        let upstream = UpstreamConfig {
            name: "plain".into(),
            base_url: "https://example.invalid".into(),
            api_key: "key-a".into(),
            supported_models: vec!["gpt-4".into(), "gpt-4o".into()],
            ..UpstreamConfig::default()
        };
        assert_eq!(
            upstream.effective_downstream_models(),
            vec!["gpt-4".to_string(), "gpt-4o".to_string()]
        );
    }

    #[test]
    fn model_mappings_detailed_flags_mapping_labels() {
        let upstream = mapped_upstream();
        let detailed = upstream.effective_downstream_models_detailed();
        assert_eq!(
            detailed,
            vec![
                DownstreamModelEntry {
                    model: "gpt-4-premium".into(),
                    from_mapping: true,
                },
                DownstreamModelEntry {
                    model: "gpt-4o".into(),
                    from_mapping: false,
                },
            ]
        );
    }

    #[test]
    fn model_mappings_detailed_preserves_mapping_casing() {
        // The admin-typed downstream label keeps its exact spelling even
        // when it differs from the canonical folded form.
        let mut upstream = mapped_upstream();
        upstream.model_mappings = vec![mapping("gpt-4", "GPT-4-Premium")];
        let detailed = upstream.effective_downstream_models_detailed();
        assert_eq!(
            detailed
                .iter()
                .find(|entry| entry.from_mapping)
                .map(|entry| entry.model.as_str()),
            Some("GPT-4-Premium")
        );
        // Unmapped route models keep their stored spelling in the detailed
        // view too (display pipeline decides folding).
        upstream.supported_models = vec!["gpt-4".into(), "DeepSeek-Chat".into()];
        let detailed = upstream.effective_downstream_models_detailed();
        assert_eq!(
            detailed
                .iter()
                .find(|entry| !entry.from_mapping)
                .map(|entry| entry.model.as_str()),
            Some("DeepSeek-Chat")
        );
    }
}

#[cfg(test)]
mod dialect_preset_tests {
    use super::*;

    fn preset_upstream() -> UpstreamConfig {
        UpstreamConfig {
            name: "presets".into(),
            base_url: "https://example.invalid".into(),
            api_key: "key-a".into(),
            dialect_preset: Some("openai".into()),
            model_dialect_presets: BTreeMap::from([
                ("glm-*".to_string(), "glm".to_string()),
                ("deepseek-*".to_string(), "deepseek".to_string()),
                ("deepseek-v4-flash-0731".to_string(), "minimax".to_string()),
            ]),
            ..UpstreamConfig::default()
        }
    }

    #[test]
    fn per_model_preset_exact_match_wins_over_wildcard_and_fallback() {
        let upstream = preset_upstream();
        // Exact canonical match beats the glm-* wildcard.
        assert_eq!(
            upstream.dialect_preset_for_model("deepseek-v4-flash-0731"),
            Some("minimax")
        );
        // Case/whitespace-insensitive canonical folding.
        assert_eq!(
            upstream.dialect_preset_for_model("DEEPSEEK-V4-FLASH-0731"),
            Some("minimax")
        );
    }

    #[test]
    fn per_model_preset_wildcard_covers_family_and_fallback_for_miss() {
        let upstream = preset_upstream();
        assert_eq!(upstream.dialect_preset_for_model("glm-5.1"), Some("glm"));
        assert_eq!(
            upstream.dialect_preset_for_model("deepseek-v4"),
            Some("deepseek")
        );
        // No matching pattern fall back to the per-upstream preset.
        assert_eq!(upstream.dialect_preset_for_model("gpt-4"), Some("openai"));
    }

    #[test]
    fn per_model_preset_empty_map_falls_back_to_per_upstream() {
        let upstream = UpstreamConfig {
            dialect_preset: Some("glm".into()),
            ..UpstreamConfig::default()
        };
        assert_eq!(upstream.dialect_preset_for_model("glm-5.1"), Some("glm"));
        assert_eq!(upstream.dialect_preset_for_model("anything"), Some("glm"));
    }
}
