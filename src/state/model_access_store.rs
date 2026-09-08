use super::model_access::{resolve_effective_models, AccessMode, AccessMutation, AccessPolicy, AllowedModels, ModelAccessSelection, ResolvedModelAccess};
use super::model_identity::ModelAliasRegistry;
use super::{ModelCatalog, PortalStore, PortalStoreError};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::io;
use tokio_postgres::{GenericClient, IsolationLevel, Row, Transaction};

pub(crate) const ACCESS_SCHEMA: &str = include_str!("../../migrations/2026-09-07-downstream-access-policies.sql");

pub(crate) async fn lock_access_writes(tx: &Transaction<'_>) -> Result<(), PortalStoreError> {
    tx.execute("SELECT pg_advisory_xact_lock(718040911)", &[]).await?;
    Ok(())
}

pub(crate) fn policy_from_row(row: &Row) -> Result<AccessPolicy, PortalStoreError> {
    let mode: String = row.get(3);
    Ok(AccessPolicy {
        downstream_id: row.get(0), subject_kind: row.get(1), owner_user_id: row.get(2),
        mode: match mode.as_str() {
            "inherit" => AccessMode::Inherit,
            "group" => AccessMode::Group,
            "deny" => AccessMode::Deny,
            _ => return Err(PortalStoreError::Db("invalid access policy mode".into())),
        },
        model_group_id: row.get(4), revision: row.get(5),
    })
}

async fn group_definitions<C: GenericClient + Sync>(client: &C) -> Result<BTreeMap<String, Vec<String>>, PortalStoreError> {
    let mut result = BTreeMap::new();
    for row in client.query("SELECT id, allowed_models FROM model_groups ORDER BY id", &[]).await? {
        let id: String = row.get(0);
        let value: serde_json::Value = row.get(1);
        let models = serde_json::from_value(value)
            .map_err(|_| PortalStoreError::Db(format!("invalid model group: {id}")))?;
        result.insert(id, models);
    }
    Ok(result)
}

fn resolve_group(groups: &BTreeMap<String, Vec<String>>, id: &str, catalog: &ModelCatalog) -> Result<AllowedModels, PortalStoreError> {
    let models = groups.get(id).ok_or_else(|| PortalStoreError::Db(format!("missing model group: {id}")))?;
    AllowedModels::from_group(models, catalog).map_err(PortalStoreError::Db)
}

async fn locked_policy(tx: &Transaction<'_>, id: &str) -> Result<AccessPolicy, PortalStoreError> {
    let row = tx.query_opt(
        "SELECT downstream_id,subject_kind,owner_user_id,mode,model_group_id,revision FROM downstream_access_policies WHERE downstream_id=$1 FOR UPDATE", &[&id],
    ).await?.ok_or(PortalStoreError::NotFound)?;
    policy_from_row(&row)
}

async fn lock_user(tx: &Transaction<'_>, id: &str) -> Result<(), PortalStoreError> {
    let row = tx.query_opt("SELECT disabled FROM portal_users WHERE id=$1 FOR UPDATE", &[&id]).await?.ok_or(PortalStoreError::NotFound)?;
    if row.get::<_, bool>(0) { return Err(PortalStoreError::Forbidden("user_disabled".into())); }
    Ok(())
}

async fn validate_selection(tx: &Transaction<'_>, selection: &ModelAccessSelection, user_id: Option<&str>) -> Result<(), PortalStoreError> {
    selection.validate().map_err(PortalStoreError::Conflict)?;
    if let Some(id) = &selection.group_id {
        let row = tx.query_opt("SELECT allowed_models FROM model_groups WHERE id=$1 FOR SHARE", &[id]).await?.ok_or(PortalStoreError::NotFound)?;
        let value: serde_json::Value = row.get(0);
        let models: Vec<String> = serde_json::from_value(value).map_err(|_| PortalStoreError::Db("invalid model group".into()))?;
        AllowedModels::from_group(&models, &ModelCatalog::new(&[], &ModelAliasRegistry::default(), true)).map_err(PortalStoreError::Db)?;
        if let Some(user) = user_id {
            if id != "basic" && tx.query_opt("SELECT 1 FROM portal_user_model_groups WHERE user_id=$1 AND model_group_id=$2", &[&user,id]).await?.is_none() {
                return Err(PortalStoreError::Forbidden("model_group_forbidden".into()));
            }
        }
    }
    Ok(())
}

async fn set_selection(tx: &Transaction<'_>, id: &str, selection: &ModelAccessSelection) -> Result<(), PortalStoreError> {
    tx.execute("UPDATE downstream_access_policies SET mode=$2,model_group_id=$3,revision=revision+1 WHERE downstream_id=$1",
        &[&id,&selection.mode.as_str(),&selection.stored_group_id()]).await?;
    Ok(())
}

async fn set_binding(tx: &Transaction<'_>, id: &str, user: &str, is_default: Option<bool>, label: Option<&str>) -> Result<(), PortalStoreError> {
    let existing = tx.query_opt("SELECT is_default FROM portal_user_downstreams WHERE downstream_id=$1 AND user_id=$2", &[&id,&user]).await?;
    let has_default = tx.query_one("SELECT EXISTS(SELECT 1 FROM portal_user_downstreams WHERE user_id=$1 AND is_default)", &[&user]).await?.get::<_,bool>(0);
    let default = is_default.unwrap_or_else(|| existing.as_ref().map(|r| r.get(0)).unwrap_or(!has_default));
    if default {
        tx.execute("UPDATE portal_user_downstreams SET is_default=FALSE WHERE user_id=$1", &[&user]).await?;
    }
    tx.execute(
        "INSERT INTO portal_user_downstreams(user_id,downstream_id,is_default,label) VALUES($1,$2,$3,$4)
         ON CONFLICT(user_id,downstream_id) DO UPDATE SET is_default=EXCLUDED.is_default,label=COALESCE(EXCLUDED.label,portal_user_downstreams.label)",
        &[&user,&id,&default,&label],
    ).await?;
    Ok(())
}

async fn remove_binding(tx: &Transaction<'_>, id: &str, user: &str) -> Result<(), PortalStoreError> {
    tx.execute("DELETE FROM portal_user_downstreams WHERE user_id=$1 AND downstream_id=$2", &[&user,&id]).await?;
    tx.execute(
        "UPDATE portal_user_downstreams SET is_default=TRUE WHERE user_id=$1
         AND downstream_id=(SELECT downstream_id FROM portal_user_downstreams WHERE user_id=$1 ORDER BY downstream_id LIMIT 1)
         AND NOT EXISTS(SELECT 1 FROM portal_user_downstreams WHERE user_id=$1 AND is_default)", &[&user],
    ).await?;
    Ok(())
}

pub(crate) async fn apply_access_mutation(tx: &Transaction<'_>, mutation: &AccessMutation) -> Result<(), PortalStoreError> {
    lock_access_writes(tx).await?;
    match mutation {
        AccessMutation::Update { downstream_id, selection, actor_user_id } => {
            if let Some(user) = actor_user_id { lock_user(tx,user).await?; }
            validate_selection(tx,selection,actor_user_id.as_deref()).await?;
            let policy = locked_policy(tx,downstream_id).await?;
            if let Some(user) = actor_user_id {
                if policy.owner_user_id.as_ref() != Some(user) { return Err(PortalStoreError::Forbidden("key_owner_mismatch".into())); }
                if policy.mode == AccessMode::Deny { return Err(PortalStoreError::Forbidden("key_denied".into())); }
            }
            if selection.mode == AccessMode::Inherit && (policy.subject_kind != "portal" || policy.owner_user_id.is_none()) {
                return Err(PortalStoreError::Conflict("inherit requires a portal owner".into()));
            }
            set_selection(tx,downstream_id,selection).await?;
        }
        AccessMutation::CreatePortal { downstream_id, user_id, label, selection } => {
            lock_user(tx,user_id).await?;
            validate_selection(tx,selection,Some(user_id)).await?;
            let count: i64 = tx.query_one("SELECT COUNT(*) FROM downstream_access_policies p JOIN portal_user_downstreams b ON b.downstream_id=p.downstream_id AND b.user_id=p.owner_user_id WHERE p.owner_user_id=$1", &[user_id]).await?.get(0);
            if count >= 10 { return Err(PortalStoreError::Conflict("key limit reached (10)".into())); }
            let policy = locked_policy(tx,downstream_id).await?;
            if policy.owner_user_id.is_some() { return Err(PortalStoreError::Conflict("key already owned".into())); }
            tx.execute("UPDATE downstream_access_policies SET subject_kind='portal',owner_user_id=$2 WHERE downstream_id=$1", &[downstream_id,user_id]).await?;
            set_selection(tx,downstream_id,selection).await?;
            set_binding(tx,downstream_id,user_id,None,label.as_deref()).await?;
        }
        AccessMutation::Bind { downstream_id,user_id,is_default,label,selection,grant_existing } => {
            lock_user(tx,user_id).await?;
            if let Some(selection) = selection { validate_selection(tx,selection,None).await?; }
            tx.query_opt("SELECT id FROM downstreams WHERE id=$1 FOR UPDATE", &[downstream_id]).await?.ok_or(PortalStoreError::NotFound)?;
            let policy = locked_policy(tx,downstream_id).await?;
            let other = tx.query_one("SELECT EXISTS(SELECT 1 FROM portal_user_downstreams WHERE downstream_id=$1 AND user_id<>$2)", &[downstream_id,user_id]).await?.get::<_,bool>(0);
            if other || policy.owner_user_id.as_ref().is_some_and(|id| id != user_id) {
                return Err(PortalStoreError::Conflict("key_owner_conflict".into()));
            }
            if *grant_existing && policy.subject_kind == "portal" && policy.owner_user_id.is_none() {
                return Err(PortalStoreError::Forbidden("owner_missing".into()));
            }
            if *grant_existing && policy.subject_kind == "direct" && policy.mode == AccessMode::Group && policy.model_group_id != "deny-all" {
                tx.execute("INSERT INTO portal_user_model_groups(user_id,model_group_id,granted_by) VALUES($1,$2,'legacy-login') ON CONFLICT DO NOTHING", &[user_id,&policy.model_group_id]).await?;
            }
            tx.execute("UPDATE downstream_access_policies SET subject_kind='portal',owner_user_id=$2,revision=revision+1 WHERE downstream_id=$1", &[downstream_id,user_id]).await?;
            if let Some(selection) = selection { set_selection(tx,downstream_id,selection).await?; }
            set_binding(tx,downstream_id,user_id,*is_default,label.as_deref()).await?;
        }
        AccessMutation::Unbind { downstream_id,user_id } => {
            tx.query_opt("SELECT id FROM portal_users WHERE id=$1 FOR UPDATE", &[user_id]).await?.ok_or(PortalStoreError::NotFound)?;
            let policy = locked_policy(tx,downstream_id).await?;
            if policy.owner_user_id.as_ref().is_some_and(|owner| owner != user_id) {
                return Err(PortalStoreError::Forbidden("key_owner_mismatch".into()));
            }
            tx.execute("UPDATE downstream_access_policies SET subject_kind='portal',owner_user_id=NULL,mode='deny',model_group_id='deny-all',revision=revision+1 WHERE downstream_id=$1", &[downstream_id]).await?;
            remove_binding(tx,downstream_id,user_id).await?;
        }
        AccessMutation::SetDefault { downstream_id,user_id } => {
            lock_user(tx,user_id).await?;
            let policy = locked_policy(tx,downstream_id).await?;
            if policy.owner_user_id.as_ref() != Some(user_id) { return Err(PortalStoreError::Forbidden("key_owner_mismatch".into())); }
            set_binding(tx,downstream_id,user_id,Some(true),None).await?;
        }
        AccessMutation::SetLabel { downstream_id,user_id,label } => {
            lock_user(tx,user_id).await?;
            let policy = locked_policy(tx,downstream_id).await?;
            if policy.owner_user_id.as_ref() != Some(user_id) { return Err(PortalStoreError::Forbidden("key_owner_mismatch".into())); }
            tx.execute("UPDATE portal_user_downstreams SET label=$3 WHERE user_id=$1 AND downstream_id=$2", &[user_id,downstream_id,label]).await?;
        }
        AccessMutation::RotatePortal { old_id,new_id,user_id } => {
            lock_user(tx,user_id).await?;
            let old = locked_policy(tx,old_id).await?;
            if old.owner_user_id.as_ref() != Some(user_id) { return Err(PortalStoreError::Forbidden("key_owner_mismatch".into())); }
            let binding = tx.query_opt("SELECT is_default,label FROM portal_user_downstreams WHERE user_id=$1 AND downstream_id=$2", &[user_id,old_id]).await?.ok_or(PortalStoreError::NotFound)?;
            let label: Option<String> = binding.get(1);
            tx.execute("UPDATE downstream_access_policies SET subject_kind='portal',owner_user_id=$2,mode=$3,model_group_id=$4,revision=revision+1 WHERE downstream_id=$1",
                &[new_id,user_id,&old.mode.as_str(),&old.model_group_id]).await?;
            set_binding(tx,new_id,user_id,Some(binding.get(0)),label.as_deref()).await?;
            remove_binding(tx,old_id,user_id).await?;
            tx.execute("UPDATE downstream_access_policies SET mode='deny',model_group_id='deny-all',revision=revision+1 WHERE downstream_id=$1", &[old_id]).await?;
        }
    }
    Ok(())
}

pub(crate) async fn migrate_access_policies(tx: &Transaction<'_>) -> io::Result<()> {
    migrate_access_policies_inner(tx).await.map_err(io::Error::other)
}

async fn migrate_access_policies_inner(tx: &Transaction<'_>) -> Result<(), PortalStoreError> {
    tx.batch_execute(ACCESS_SCHEMA).await?;
    let mut groups = group_definitions(tx).await?;
    let catalog = ModelCatalog::new(&[], &ModelAliasRegistry::default(), true);
    let rows = tx.query(
        "SELECT d.id, COALESCE(d.model_group_id, 'deny-all'), d.is_portal_key,
            ARRAY(SELECT b.user_id FROM portal_user_downstreams b WHERE b.downstream_id=d.id ORDER BY b.user_id),
            ARRAY(SELECT COALESCE(b.model_group_id, 'basic') FROM portal_user_downstreams b WHERE b.downstream_id=d.id ORDER BY b.user_id)
         FROM downstreams d WHERE NOT EXISTS (SELECT 1 FROM downstream_access_policies p WHERE p.downstream_id=d.id)
         AND NOT EXISTS (SELECT 1 FROM downstream_access_migrations m WHERE m.downstream_id=d.id AND m.migration_version=1)
         ORDER BY d.id", &[],
    ).await?;
    let migrated_count = rows.len();
    for row in rows {
        let id: String = row.get(0);
        let legacy_group: String = row.get(1);
        let portal_key: bool = row.get(2);
        let owners: Vec<String> = row.get(3);
        let binding_groups: Vec<String> = row.get(4);
        let portal = portal_key || !owners.is_empty();
        let owner = (owners.len() == 1).then(|| owners[0].clone());
        let mut classification = "preserved";
        let key_models = resolve_group(&groups, &legacy_group, &catalog);
        let effective = if owners.len() > 1 {
            classification = "ownership_conflict";
            AllowedModels::None
        } else if portal && owner.is_none() {
            classification = "orphan";
            AllowedModels::None
        } else {
            match key_models {
                Ok(key_models) => {
                    if let Some(binding_group) = binding_groups.first() {
                        match resolve_group(&groups, binding_group, &catalog) {
                            Ok(models) => key_models.intersection(&models),
                            Err(_) => { classification = "invalid_group"; AllowedModels::None }
                        }
                    } else { key_models }
                }
                Err(_) => { classification = "invalid_group"; AllowedModels::None }
            }
        };
        if portal && effective == AllowedModels::None && classification == "preserved" {
            classification = "review_required";
        }
        let group_id = if effective == AllowedModels::None {
            "deny-all".to_owned()
        } else {
            let preferred = binding_groups.first().unwrap_or(&legacy_group);
            if resolve_group(&groups, preferred, &catalog).ok().as_ref() == Some(&effective) {
                preferred.clone()
            } else if resolve_group(&groups, &legacy_group, &catalog).ok().as_ref() == Some(&effective) {
                legacy_group.clone()
            } else {
                let models = effective.legacy_projection();
                let encoded = serde_json::to_vec(&models).map_err(|e| PortalStoreError::Db(e.to_string()))?;
                let digest = format!("{:x}", Sha256::digest(&encoded));
                let group_id = format!("migrated-access-{}", &digest[..24]);
                tx.execute(
                    "INSERT INTO model_groups(id,name,description,allowed_models) VALUES($1,$1,'Migrated effective key access',$2) ON CONFLICT(id) DO NOTHING",
                    &[&group_id, &json!(models)],
                ).await?;
                let stored: serde_json::Value = tx.query_one("SELECT allowed_models FROM model_groups WHERE id=$1", &[&group_id]).await?.get(0);
                if stored != json!(models) {
                    return Err(PortalStoreError::Conflict("migration group id already has different models".into()));
                }
                groups.insert(group_id.clone(), models);
                group_id
            }
        };
        let mode = if effective == AllowedModels::None { AccessMode::Deny } else { AccessMode::Group };
        let policy = AccessPolicy {
            downstream_id: id.clone(), subject_kind: if portal { "portal" } else { "direct" }.into(),
            owner_user_id: owner.clone(), mode, model_group_id: group_id.clone(), revision: 1,
        };
        tx.execute(
            "INSERT INTO downstream_access_policies(downstream_id,subject_kind,owner_user_id,mode,model_group_id) VALUES($1,$2,$3,$4,$5)",
            &[&id,&policy.subject_kind,&owner,&mode.as_str(),&group_id],
        ).await?;
        if let Some(user) = &owner {
            if effective != AllowedModels::None {
                tx.execute(
                    "INSERT INTO portal_user_model_groups(user_id,model_group_id,granted_by) VALUES($1,$2,'access-migration-v1') ON CONFLICT DO NOTHING",
                    &[user,&group_id],
                ).await?;
            }
        }
        tx.execute(
            "INSERT INTO downstream_access_migrations(downstream_id,migration_version,classification,before_policy,after_policy) VALUES($1,1,$2,$3,$4)",
            &[&id,&classification,&json!({"key_group_id":legacy_group,"owner_ids":owners,"binding_group_ids":binding_groups,"effective_models":effective.legacy_projection()}),&json!(policy)],
        ).await?;
    }
    tracing::info!(migrated_count, "downstream access policy migration complete");
    Ok(())
}

impl PortalStore {
    pub async fn mutate_access(&self, mutation: &AccessMutation) -> Result<(), PortalStoreError> {
        let mut client = self.get_client().await?;
        let tx = client.transaction().await?;
        apply_access_mutation(&tx,mutation).await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn access_policies(&self, ids: &[String]) -> Result<HashMap<String, AccessPolicy>, PortalStoreError> {
        let client = self.get_client().await?;
        let rows = client.query(
            "SELECT downstream_id,subject_kind,owner_user_id,mode,model_group_id,revision FROM downstream_access_policies WHERE downstream_id=ANY($1)", &[&ids],
        ).await?;
        rows.iter().map(|row| policy_from_row(row).map(|p| (p.downstream_id.clone(),p))).collect()
    }

    pub async fn resolve_key_access(&self, ids: &[String], catalog: &ModelCatalog) -> Result<HashMap<String, ResolvedModelAccess>, PortalStoreError> {
        let mut client = self.get_client().await?;
        let tx = client.build_transaction().isolation_level(IsolationLevel::RepeatableRead).read_only(true).start().await?;
        let rows = tx.query(
            "SELECT p.downstream_id,p.subject_kind,p.owner_user_id,p.mode,p.model_group_id,p.revision,u.disabled,
                (SELECT COUNT(*) FROM portal_user_downstreams b WHERE b.downstream_id=p.downstream_id),
                EXISTS(SELECT 1 FROM portal_user_downstreams b WHERE b.downstream_id=p.downstream_id AND b.user_id=p.owner_user_id)
             FROM downstream_access_policies p LEFT JOIN portal_users u ON u.id=p.owner_user_id
             WHERE p.downstream_id=ANY($1)", &[&ids],
        ).await?;
        if rows.len() != ids.iter().collect::<std::collections::HashSet<_>>().len() {
            return Err(PortalStoreError::Db("downstream access policy missing".into()));
        }
        let owner_ids: Vec<String> = rows.iter().filter_map(|row| row.get::<_, Option<String>>(2)).collect();
        let mut grants: HashMap<String, Vec<String>> = owner_ids.iter().map(|id| (id.clone(),vec!["basic".into()])).collect();
        for row in tx.query("SELECT user_id,model_group_id FROM portal_user_model_groups WHERE user_id=ANY($1) ORDER BY model_group_id", &[&owner_ids]).await? {
            grants.entry(row.get(0)).or_default().push(row.get(1));
        }
        let groups = group_definitions(&tx).await?;
        let mut resolved = HashMap::new();
        for row in rows {
            let policy = policy_from_row(&row)?;
            let portal = policy.subject_kind == "portal";
            let disabled: Option<bool> = row.get(6);
            let count: i64 = row.get(7);
            let bound: bool = row.get(8);
            let owner_valid = portal && disabled == Some(false) && count == 1 && bound;
            let mut user_group_ids = policy.owner_user_id.as_ref().and_then(|id| grants.get(id)).cloned().unwrap_or_default();
            user_group_ids.sort();
            user_group_ids.dedup();
            let mut ceiling = AllowedModels::None;
            if owner_valid {
                for id in &user_group_ids { ceiling = ceiling.union(&resolve_group(&groups,id,catalog)?); }
            }
            let group = if policy.mode == AccessMode::Group { resolve_group(&groups,&policy.model_group_id,catalog)? } else { AllowedModels::None };
            let allowed = resolve_effective_models(policy.mode,portal,owner_valid.then_some(&ceiling),&group);
            let reason = if portal && count > 1 { Some("ownership_conflict") }
                else if portal && disabled == Some(true) { Some("user_disabled") }
                else if portal && !owner_valid { Some("owner_missing") }
                else if policy.mode == AccessMode::Deny { Some("key_denied") }
                else if allowed == AllowedModels::None { Some("no_authorized_models") }
                else { None };
            resolved.insert(policy.downstream_id.clone(),ResolvedModelAccess {
                policy: Some(policy),allowed,user_group_ids,reason: reason.map(str::to_owned),
            });
        }
        tx.commit().await?;
        Ok(resolved)
    }

    pub async fn resolve_user_access(&self, user_id: &str, catalog: &ModelCatalog) -> Result<ResolvedModelAccess, PortalStoreError> {
        let mut client = self.get_client().await?;
        let tx = client.build_transaction().isolation_level(IsolationLevel::RepeatableRead).read_only(true).start().await?;
        let user = tx.query_opt("SELECT disabled FROM portal_users WHERE id=$1", &[&user_id]).await?.ok_or(PortalStoreError::NotFound)?;
        if user.get::<_, bool>(0) { return Err(PortalStoreError::Forbidden("user_disabled".into())); }
        let mut ids = vec!["basic".to_owned()];
        for row in tx.query("SELECT model_group_id FROM portal_user_model_groups WHERE user_id=$1 ORDER BY model_group_id", &[&user_id]).await? { ids.push(row.get(0)); }
        ids.sort(); ids.dedup();
        let groups = group_definitions(&tx).await?;
        let mut allowed = AllowedModels::None;
        for id in &ids { allowed = allowed.union(&resolve_group(&groups,id,catalog)?); }
        tx.commit().await?;
        Ok(ResolvedModelAccess { policy:None,allowed,user_group_ids:ids,reason:None })
    }
}
