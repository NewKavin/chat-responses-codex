use super::normalize::normalized_default_model_context;
use super::types::*;
use crate::state::AppState;
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
        self.context_config_for_model_with(model, true)
    }

    pub fn context_config_for_model_with(
        &self,
        model: &str,
        case_insensitive: bool,
    ) -> Option<ModelContextConfig> {
        self.context_config_for_model_with_profile_and_case(model, None, case_insensitive)
    }

    pub fn context_config_for_model_with_profile(
        &self,
        model: &str,
        profile: Option<&GlobalContextProfile>,
    ) -> Option<ModelContextConfig> {
        self.context_config_for_model_with_profile_and_case(model, profile, true)
    }

    pub fn context_config_for_model_with_profile_and_case(
        &self,
        model: &str,
        profile: Option<&GlobalContextProfile>,
        case_insensitive: bool,
    ) -> Option<ModelContextConfig> {
        let candidate = self.resolved_model_name_with(model, case_insensitive)?;
        for candidate in [candidate, model.trim().to_string()] {
            let slug_matches = |config: &ModelContextConfig| {
                if case_insensitive {
                    super::models_equivalent(&config.slug, &candidate)
                } else {
                    config.slug.trim() == candidate
                }
            };
            if let Some(config) = self.model_contexts.iter().find(|config| slug_matches(config)) {
                return Some(config.clone());
            }

            if let Some(profile) = profile {
                if let Some(config) = profile
                    .model_contexts
                    .iter()
                    .find(|config| slug_matches(config))
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
                max_output_tokens: config.max_output_tokens,
                context_group: config.context_group.clone(),
            })
            .or_else(|| {
                profile
                    .and_then(|profile| profile.default_model_context.as_ref())
                    .map(|config| ModelContextConfig {
                        slug: model.trim().to_string(),
                        context_limit: config.context_limit,
                        output_reserve: config.output_reserve,
                        max_output_tokens: config.max_output_tokens,
                        context_group: config.context_group.clone(),
                    })
            })
    }

    pub fn context_fallback_model_for(
        &self,
        model: &str,
        minimum_context_limit: u32,
    ) -> Option<String> {
        self.context_fallback_model_for_with_profile(model, minimum_context_limit, None)
    }

    pub fn context_fallback_model_for_with_profile(
        &self,
        model: &str,
        minimum_context_limit: u32,
        profile: Option<&GlobalContextProfile>,
    ) -> Option<String> {
        self.context_fallback_model_for_with_profile_and_case(
            model,
            minimum_context_limit,
            profile,
            true,
        )
    }

    pub fn context_fallback_model_for_with_profile_and_case(
        &self,
        model: &str,
        minimum_context_limit: u32,
        profile: Option<&GlobalContextProfile>,
        case_insensitive: bool,
    ) -> Option<String> {
        let current = self.context_config_for_model_with_profile_and_case(
            model,
            profile,
            case_insensitive,
        )?;

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
            .resolved_model_name_with(model, case_insensitive)
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
                if let Some(resolved) =
                    self.resolved_model_name_with(&candidate.slug, case_insensitive)
                {
                    if resolved.trim() != current_resolved.trim() {
                        return Some(resolved);
                    }
                }
            }
        }

        for candidate in candidates {
            if let Some(resolved) =
                self.resolved_model_name_with(&candidate.slug, case_insensitive)
            {
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
