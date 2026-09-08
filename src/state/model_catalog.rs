use super::model_identity::{canonical_model_id, model_identity_key_with, ModelAliasRegistry};
use super::UpstreamConfig;
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[serde(transparent)]
pub struct ModelId(pub(crate) String);

impl ModelId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug)]
pub struct PublishedRoute {
    pub upstream_id: String,
    pub wire_model: String,
    pub exposed_model: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct PublishedModel {
    pub id: ModelId,
    pub name: String,
    #[serde(skip)]
    pub routes: Vec<PublishedRoute>,
}

#[derive(Clone, Debug)]
pub struct ModelCatalog {
    models: Vec<PublishedModel>,
    aliases: ModelAliasRegistry,
    explicit_labels: HashSet<String>,
    case_insensitive: bool,
}

impl ModelCatalog {
    pub fn new(
        upstreams: &[UpstreamConfig],
        aliases: &ModelAliasRegistry,
        case_insensitive: bool,
    ) -> Self {
        let entries: Vec<_> = upstreams.iter().filter(|u| u.active).flat_map(|upstream| {
            upstream.effective_downstream_models_detailed().into_iter()
                .map(move |entry| (upstream, entry))
        }).collect();
        let explicit_labels = entries.iter().filter(|(_, e)| e.from_mapping)
            .map(|(_, e)| canonical_model_id(&e.model)).collect::<HashSet<_>>();
        let mut catalog = Self { models: Vec::new(), aliases: aliases.clone(), explicit_labels, case_insensitive };
        let mut grouped: BTreeMap<String, (PublishedModel, bool)> = BTreeMap::new();
        for (upstream, entry) in entries {
            let raw = entry.model.trim();
            if raw.is_empty() || matches!(raw, "*" | "__none__") {
                continue;
            }
            let name = if entry.from_mapping {
                raw.to_owned()
            } else if let Some(canonical) = aliases.resolve_alias(raw) {
                canonical.to_owned()
            } else if case_insensitive {
                canonical_model_id(raw)
            } else {
                raw.to_owned()
            };
            let key = model_identity_key_with(&name, case_insensitive);
            let route = PublishedRoute {
                upstream_id: upstream.id.clone(),
                exposed_model: raw.to_owned(),
                wire_model: upstream.resolved_model_name_with(raw, case_insensitive)
                    .unwrap_or_else(|| raw.to_owned()),
            };
            let id = catalog.resolve_public_model_id(&name);
            let (model, mapped) = grouped.entry(key).or_insert_with(|| (
                PublishedModel { id, name: name.clone(), routes: Vec::new() }, entry.from_mapping,
            ));
            if (entry.from_mapping && !*mapped) || (entry.from_mapping == *mapped && name < model.name) {
                model.name = name;
                *mapped = entry.from_mapping;
            }
            model.routes.push(route);
        }
        catalog.models = grouped.into_values().map(|(model, _)| model).collect();
        catalog.models.sort_by(|a, b| a.name.cmp(&b.name));
        catalog
    }

    pub fn models(&self) -> &[PublishedModel] {
        &self.models
    }

    pub fn find(&self, name: &str) -> Option<&PublishedModel> {
        self.models.iter().find(|model| model.name == name.trim()).or_else(|| {
            let canonical = self.aliases.resolve_alias(name).unwrap_or(name).trim();
            self.models.iter().find(|model| {
                model.name == canonical || (self.case_insensitive && model.id == self.resolve_public_model_id(name))
            })
        })
    }

    pub fn public_name_for_route(&self, upstream_id: &str, exposed_model: &str) -> Option<&str> {
        self.models.iter().find(|model| model.routes.iter().any(|route| {
            route.upstream_id == upstream_id && route.exposed_model == exposed_model
        })).map(|model| model.name.as_str())
    }

    pub fn route_context(&self, snapshot: &super::PersistedState, route: &PublishedRoute) -> Option<super::ModelContextConfig> {
        let upstream = snapshot.upstreams.iter().find(|upstream| upstream.id == route.upstream_id)?;
        let base = super::normalize_context_profile_base_url(&upstream.base_url);
        upstream.context_config_for_model_with_profile_and_case(
            &route.exposed_model, snapshot.global_context_profiles.get(&base), self.case_insensitive,
        )
    }

    pub fn resolve_public_model_id(&self, name: &str) -> ModelId {
        let name = super::codex_subagent_base_model(name).unwrap_or(name).trim();
        self.resolve_group_model_id(name)
    }

    pub fn normalize_request_name(&self, name: &str) -> String {
        if self.explicit_labels.contains(&canonical_model_id(name)) { name.to_owned() }
        else { self.aliases.resolve_alias(name).unwrap_or(name).to_owned() }
    }

    pub fn resolve_group_model_id(&self, name: &str) -> ModelId {
        let name = name.trim();
        let key = canonical_model_id(name);
        if self.explicit_labels.contains(&key) {
            return ModelId(key);
        }
        ModelId(canonical_model_id(self.aliases.resolve_alias(name).unwrap_or(name)))
    }

    pub fn allows_legacy(&self, allowed: &[String], name: &str) -> bool {
        if allowed.is_empty() || allowed.iter().any(|value| value.trim() == "*") {
            return true;
        }
        let requested = self.resolve_public_model_id(name);
        !requested.0.is_empty() && allowed.iter().any(|value| {
            let value = value.trim();
            !value.is_empty() && value != "__none__" && self.resolve_group_model_id(value) == requested
        })
    }

    pub fn names_for_legacy(&self, allowed: &[String]) -> Vec<String> {
        self.models.iter().filter(|model| self.allows_legacy(allowed, &model.name))
            .map(|model| model.name.clone()).collect()
    }
}
