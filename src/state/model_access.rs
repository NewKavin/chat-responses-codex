use super::{ModelCatalog, ModelId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AllowedModels {
    All,
    None,
    Models(BTreeSet<ModelId>),
}

impl AllowedModels {
    pub fn from_group(models: &[String], catalog: &ModelCatalog) -> Result<Self, String> {
        if models.is_empty() || models.iter().any(|name| name.trim().is_empty()) {
            return Err("model group must contain nonempty model names".into());
        }
        if models.iter().any(|name| name.trim() == "__none__") {
            return if models.len() == 1 { Ok(Self::None) } else {
                Err("denial sentinel cannot be combined with model names".into())
            };
        }
        if models.iter().any(|name| name.trim() == "*") {
            return Ok(Self::All);
        }
        Ok(Self::Models(models.iter().map(|name| catalog.resolve_group_model_id(name)).collect()))
    }

    pub fn from_legacy(models: &[String], catalog: &ModelCatalog) -> Self {
        if models.is_empty() { Self::All } else {
            Self::from_group(models, catalog).unwrap_or(Self::None)
        }
    }

    pub fn union(&self, other: &Self) -> Self {
        match (self, other) {
            (Self::All, _) | (_, Self::All) => Self::All,
            (Self::None, other) | (other, Self::None) => other.clone(),
            (Self::Models(left), Self::Models(right)) => Self::Models(left.union(right).cloned().collect()),
        }
    }

    pub fn intersection(&self, other: &Self) -> Self {
        match (self, other) {
            (Self::None, _) | (_, Self::None) => Self::None,
            (Self::All, other) | (other, Self::All) => other.clone(),
            (Self::Models(left), Self::Models(right)) => {
                let models: BTreeSet<_> = left.intersection(right).cloned().collect();
                if models.is_empty() { Self::None } else { Self::Models(models) }
            }
        }
    }

    pub fn allows(&self, id: &ModelId) -> bool {
        match self {
            Self::All => true,
            Self::None => false,
            Self::Models(models) => models.contains(id),
        }
    }

    pub fn legacy_projection(&self) -> Vec<String> {
        match self {
            Self::All => vec!["*".into()],
            Self::None => vec!["__none__".into()],
            Self::Models(models) => models.iter().map(|id| id.as_str().to_owned()).collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessMode { Inherit, Group, Deny }

impl AccessMode {
    pub fn as_str(self) -> &'static str {
        match self { Self::Inherit => "inherit", Self::Group => "group", Self::Deny => "deny" }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelAccessSelection {
    pub mode: AccessMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
}

impl Default for ModelAccessSelection {
    fn default() -> Self { Self { mode: AccessMode::Inherit, group_id: None } }
}

impl ModelAccessSelection {
    pub fn validate(&self) -> Result<(), String> {
        match (&self.mode, &self.group_id) {
            (AccessMode::Group, Some(id)) if !id.trim().is_empty() => Ok(()),
            (AccessMode::Inherit | AccessMode::Deny, None) => Ok(()),
            _ => Err("group mode requires group_id; inherit and deny must not include it".into()),
        }
    }

    pub fn from_group_id(id: &str) -> Self {
        if id == "deny-all" { Self { mode: AccessMode::Deny, group_id: None } }
        else { Self { mode: AccessMode::Group, group_id: Some(id.to_owned()) } }
    }

    pub fn stored_group_id(&self) -> &str { self.group_id.as_deref().unwrap_or("deny-all") }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccessPolicy {
    pub downstream_id: String,
    pub subject_kind: String,
    pub owner_user_id: Option<String>,
    pub mode: AccessMode,
    pub model_group_id: String,
    pub revision: i64,
}

impl AccessPolicy {
    pub fn selection(&self) -> ModelAccessSelection {
        ModelAccessSelection {
            mode: self.mode,
            group_id: (self.mode == AccessMode::Group).then(|| self.model_group_id.clone()),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ResolvedModelAccess {
    pub policy: Option<AccessPolicy>,
    pub allowed: AllowedModels,
    pub user_group_ids: Vec<String>,
    pub reason: Option<String>,
}

#[derive(Clone, Debug)]
pub enum AccessMutation {
    Update {
        downstream_id: String,
        selection: ModelAccessSelection,
        actor_user_id: Option<String>,
    },
    CreatePortal {
        downstream_id: String,
        user_id: String,
        label: Option<String>,
        selection: ModelAccessSelection,
    },
    Bind {
        downstream_id: String,
        user_id: String,
        is_default: Option<bool>,
        label: Option<String>,
        selection: Option<ModelAccessSelection>,
        grant_existing: bool,
    },
    Unbind { downstream_id: String, user_id: String },
    SetDefault { downstream_id: String, user_id: String },
    SetLabel { downstream_id: String, user_id: String, label: Option<String> },
    RotatePortal { old_id: String, new_id: String, user_id: String },
}

pub fn resolve_effective_models(
    mode: AccessMode,
    portal: bool,
    owner: Option<&AllowedModels>,
    group: &AllowedModels,
) -> AllowedModels {
    if mode == AccessMode::Deny { return AllowedModels::None; }
    if portal {
        match (mode, owner) {
            (AccessMode::Inherit, Some(ceiling)) => ceiling.clone(),
            (AccessMode::Group, Some(ceiling)) => ceiling.intersection(group),
            _ => AllowedModels::None,
        }
    } else if mode == AccessMode::Group {
        group.clone()
    } else {
        AllowedModels::None
    }
}

impl super::AppState {
    pub async fn patch_downstream_account(
        &self, id: &str, updates: &serde_json::Map<String, serde_json::Value>,
    ) -> std::io::Result<super::DownstreamConfig> {
        let selection = super::parse_model_access_update(updates)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput,e))?;
        let _guard = self.config_persist_lock.lock().await;
        let mut state = self.inner.lock().await;
        let mut candidate = state.clone();
        let downstream = std::sync::Arc::make_mut(&mut candidate.downstreams).iter_mut().find(|d| d.id==id)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound,"downstream not found"))?;
        super::apply_downstream_updates(downstream,updates)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput,e))?;
        let result = downstream.clone();
        if let Some(selection) = selection {
            let postgres = self.postgres.as_ref().ok_or_else(|| std::io::Error::new(
                std::io::ErrorKind::Unsupported,"model access policies require PostgreSQL",
            ))?;
            postgres.replace_state_with_access(&candidate,&[AccessMutation::Update {
                downstream_id:id.into(),selection,actor_user_id:None,
            }]).await?;
        } else {
            self.config_store.persist_config(&candidate).await?;
        }
        state.downstreams = candidate.downstreams;
        Ok(result)
    }

    async fn mutate_config_and_access<T>(
        &self,
        mutations: &[AccessMutation],
        change: impl FnOnce(&mut super::PersistedState) -> std::io::Result<T>,
    ) -> std::io::Result<T> {
        let postgres = self.postgres.as_ref().ok_or_else(|| std::io::Error::new(
            std::io::ErrorKind::Unsupported, "portal access policies require PostgreSQL",
        ))?;
        let _guard = self.config_persist_lock.lock().await;
        let mut state = self.inner.lock().await;
        let mut candidate = state.clone();
        let result = change(&mut candidate)?;
        super::validate_downstream_plaintext_pairs(&mut candidate);
        postgres.replace_state_with_access(&candidate, mutations).await?;
        state.downstreams = candidate.downstreams;
        Ok(result)
    }

    pub async fn insert_portal_downstream(
        &self,
        mut downstream: super::DownstreamConfig,
        user_id: &str,
        label: Option<String>,
        selection: ModelAccessSelection,
    ) -> std::io::Result<()> {
        downstream.is_portal_key = true;
        downstream.model_group_id = Some("deny-all".into());
        let mutation = AccessMutation::CreatePortal {
            downstream_id: downstream.id.clone(), user_id: user_id.into(), label, selection,
        };
        self.mutate_config_and_access(&[mutation], |state| {
            if state.downstreams.iter().any(|d| d.id == downstream.id) {
                return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "downstream already exists"));
            }
            std::sync::Arc::make_mut(&mut state.downstreams).push(downstream);
            Ok(())
        }).await
    }

    pub async fn update_downstream_with_access(
        &self,
        downstream_id: &str,
        downstream: super::DownstreamConfig,
        selection: ModelAccessSelection,
        actor_user_id: Option<String>,
    ) -> std::io::Result<bool> {
        let mutation = AccessMutation::Update { downstream_id: downstream_id.into(), selection, actor_user_id };
        self.mutate_config_and_access(&[mutation], |state| {
            let entry = std::sync::Arc::make_mut(&mut state.downstreams).iter_mut().find(|d| d.id == downstream_id)
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "downstream not found"))?;
            *entry = downstream;
            entry.id = downstream_id.to_owned();
            Ok(true)
        }).await
    }

    pub async fn rotate_portal_downstream(
        &self, old_id: &str, replacement: super::DownstreamConfig, user_id: &str,
    ) -> std::io::Result<()> {
        let mutation = AccessMutation::RotatePortal { old_id:old_id.into(),new_id:replacement.id.clone(),user_id:user_id.into() };
        self.mutate_config_and_access(&[mutation], |state| {
            let entries = std::sync::Arc::make_mut(&mut state.downstreams);
            let old = entries.iter_mut().find(|d| d.id == old_id)
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "downstream not found"))?;
            let mut new_key = old.clone();
            new_key.id = replacement.id;
            new_key.hash = replacement.hash;
            new_key.plaintext_key = replacement.plaintext_key;
            new_key.plaintext_key_prefix = replacement.plaintext_key_prefix;
            new_key.is_portal_key = true;
            old.active = false;
            entries.push(new_key);
            Ok(())
        }).await
    }

    /// T15 删组一致性：把内存快照里所有仍引用 `deleted_group_id` 的下游
    /// 重置为 deny-all 哨兵组并持久化，与 DB 层外键 `ON DELETE SET DEFAULT`
    /// 的行为保持一致。必须在 portal store 删组之前调用：DB 行会由外键
    /// 自动落到 deny-all，如果内存不同步，下一次全量 sync（例如创建上游）
    /// 会把过期的 group_id 写回，触发 FK 冲突并对外表现为 "db error"。
    pub async fn retarget_downstreams_after_group_delete(
        &self,
        deleted_group_id: &str,
    ) -> std::io::Result<usize> {
        let _guard = self.config_persist_lock.lock().await;
        let mut state = self.inner.lock().await;
        let mut candidate = state.clone();
        let mut changed = 0usize;
        for downstream in std::sync::Arc::make_mut(&mut candidate.downstreams) {
            if downstream.model_group_id.as_deref() == Some(deleted_group_id) {
                downstream.model_group_id = Some("deny-all".to_string());
                changed += 1;
            }
        }
        if changed > 0 {
            self.config_store.persist_config(&candidate).await?;
            state.downstreams = candidate.downstreams;
        }
        Ok(changed)
    }

    pub async fn resolved_model_access(&self, downstream: &super::DownstreamConfig) -> Result<ResolvedModelAccess, String> {
        let catalog = self.model_catalog().await;
        self.resolved_model_access_with_catalog(downstream,&catalog).await
    }

    pub async fn resolved_model_access_with_catalog(&self, downstream: &super::DownstreamConfig, catalog: &ModelCatalog) -> Result<ResolvedModelAccess, String> {
        if let Some(store) = self.portal_store() {
            let mut result = store.resolve_key_access(&[downstream.id.clone()], catalog).await.map_err(|e| e.to_string())?;
            result.remove(&downstream.id).ok_or_else(|| "downstream access policy missing".into())
        } else {
            Ok(ResolvedModelAccess {
                policy:None,allowed:AllowedModels::from_legacy(&downstream.model_allowlist,catalog),
                user_group_ids:Vec::new(),reason:None,
            })
        }
    }

    pub async fn revoke_portal_downstream(&self, downstream_id: &str, user_id: &str) -> std::io::Result<()> {
        let mutation = AccessMutation::Unbind { downstream_id: downstream_id.into(), user_id:user_id.into() };
        self.mutate_config_and_access(&[mutation], |state| {
            let downstream = std::sync::Arc::make_mut(&mut state.downstreams).iter_mut().find(|d| d.id == downstream_id)
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound,"downstream not found"))?;
            downstream.active = false;
            Ok(())
        }).await
    }
}
