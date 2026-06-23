use crate::state::AppState;
use super::normalize::normalized_default_model_context;
use super::types::*;
use std::collections::HashMap;

pub(super) fn normalize_context_profile_base_url(base_url: &str) -> String {
    base_url.trim().trim_end_matches('/').to_string()
}

pub(super) fn normalize_global_context_profiles_for_storage(
    profiles: HashMap<String, GlobalContextProfile>,
) -> HashMap<String, GlobalContextProfile> {
    profiles
        .into_iter()
        .filter_map(|(base_url, mut profile)| {
            let base_url = normalize_context_profile_base_url(&base_url);
            if base_url.is_empty() {
                return None;
            }
            profile.normalize_for_storage();
            Some((base_url, profile))
        })
        .collect::<HashMap<_, _>>()
}

impl GlobalContextProfile {
    pub fn normalize_for_storage(&mut self) {
        self.model_contexts =
            super::normalize::normalized_model_contexts(std::mem::take(&mut self.model_contexts));
        self.default_model_context =
            normalized_default_model_context(self.default_model_context.take());
    }
}

impl UpstreamConfig {
    pub fn context_config_for_model(&self, model: &str) -> Option<ModelContextConfig> {
        self.context_config_for_model_with_profile(model, None)
    }

    pub fn context_config_for_model_with_profile(
        &self,
        model: &str,
        profile: Option<&GlobalContextProfile>,
    ) -> Option<ModelContextConfig> {
        let candidate = self.resolved_model_name(model)?;
        for candidate in [candidate, model.trim().to_string()] {
            if let Some(config) = self
                .model_contexts
                .iter()
                .find(|config| config.slug.trim() == candidate)
            {
                return Some(config.clone());
            }

            if let Some(profile) = profile {
                if let Some(config) = profile
                    .model_contexts
                    .iter()
                    .find(|config| config.slug.trim() == candidate)
                {
                    return Some(config.clone());
                }
            }
        }

        self.default_model_context
            .as_ref()
            .map(|config| ModelContextConfig {
                slug: model.trim().to_string(),
                context_limit: config.context_limit,
                output_reserve: config.output_reserve,
                context_group: config.context_group.clone(),
            })
            .or_else(|| {
                profile
                    .and_then(|profile| profile.default_model_context.as_ref())
                    .map(|config| ModelContextConfig {
                        slug: model.trim().to_string(),
                        context_limit: config.context_limit,
                        output_reserve: config.output_reserve,
                        context_group: config.context_group.clone(),
                    })
            })
    }

    pub fn context_fallback_model_for(&self, model: &str, minimum_context_limit: u32) -> Option<String> {
        self.context_fallback_model_for_with_profile(model, minimum_context_limit, None)
    }

    pub fn context_fallback_model_for_with_profile(
        &self,
        model: &str,
        minimum_context_limit: u32,
        profile: Option<&GlobalContextProfile>,
    ) -> Option<String> {
        let current = self.context_config_for_model_with_profile(model, profile)?;

        let mut candidate_contexts = HashMap::new();

        if let Some(profile) = profile {
            for config in &profile.model_contexts {
                candidate_contexts.insert(config.slug.trim().to_string(), config.clone());
            }
        }

        for config in &self.model_contexts {
            candidate_contexts.insert(config.slug.trim().to_string(), config.clone());
        }

        let group = current.context_group.trim();
        if group.is_empty() {
            return None;
        }
        let current_resolved = self
            .resolved_model_name(model)
            .unwrap_or_else(|| model.to_string());

        let mut candidates = candidate_contexts
            .values()
            .filter(|config| {
                config.context_group.trim() == group && config.context_limit > current.context_limit
            })
            .cloned()
            .collect::<Vec<_>>();
        candidates.sort_by_key(|config| config.context_limit);

        for candidate in &candidates {
            if candidate.context_limit >= minimum_context_limit {
                if let Some(resolved) = self.resolved_model_name(&candidate.slug) {
                    if resolved.trim() != current_resolved.trim() {
                        return Some(resolved);
                    }
                }
            }
        }

        for candidate in candidates {
            if let Some(resolved) = self.resolved_model_name(&candidate.slug) {
                if resolved.trim() != current_resolved.trim() {
                    return Some(resolved);
                }
            }
        }

        None
    }
}

impl AppState {
    pub async fn global_context_profile_for_upstream_base_url(
        &self,
        base_url: &str,
    ) -> Option<GlobalContextProfile> {
        let base_url = normalize_context_profile_base_url(base_url);
        if base_url.is_empty() {
            return None;
        }

        let state = self.inner.lock().await;
        state.global_context_profiles.get(&base_url).cloned()
    }
}
