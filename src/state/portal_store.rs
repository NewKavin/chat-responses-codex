//! Portal OIDC durable store (design §3, T1).
//!
//! Postgres-only.  `AppState::portal_store()` is `None` in file mode; every
//! OIDC endpoint then answers 503 `oidc_requires_durable_store` instead of
//! degrading silently.  The four tables are created by `SCHEMA_SQL` in
//! `postgres.rs`; `portal_sessions.sid` stores only the SHA-256 hash of the
//! random cookie value, never the value itself.

use bb8::Pool;
use bb8_postgres::PostgresConnectionManager;
use tokio_postgres::NoTls;
use super::{AccessMutation, AccessMode, ModelAccessSelection};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

type Manager = PostgresConnectionManager<NoTls>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortalUser {
    pub id: String,
    pub email: String,
    pub display_name: Option<String>,
    pub username: Option<String>,
    pub disabled: bool,
    /// unix seconds
    pub created_at: i64,
    /// unix seconds
    pub last_login_at: Option<i64>,
    /// first bound identity (provider, subject), when any
    pub provider: Option<String>,
    pub subject: Option<String>,
    pub binding_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortalDownstreamBinding {
    pub downstream_id: String,
    pub is_default: bool,
    pub label: Option<String>,  // 新增：兼容 NULL
    pub model_group_id: String,  // 新增：模型分组（默认 'basic'）
    pub model_access: ModelAccessSelection,
}

impl PortalDownstreamBinding {
    /// 获取 label，现有数据返回默认值
    pub fn label(&self) -> &str {
        self.label.as_deref().unwrap_or("Default Key")
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PortalDownstreamBindingWithLabel {
    pub downstream_id: String,
    pub is_default: bool,
    pub label: String,  // 前端总是收到非空 label
    pub model_group_id: String,  // 模型分组
    pub model_group_name: Option<String>,  // 模型分组名称
    pub created_at: i64,  // Unix timestamp
    pub usage_count: i64,  // 使用次数（从 response_history 统计）
    pub plaintext_key: Option<String>,  // 密钥明文（仅对绑定 owner 可见，可随时回看/复制）
    pub model_access: ModelAccessSelection,
    pub access_revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortalSession {
    /// SHA-256 hash of the cookie value
    pub sid: String,
    pub user_id: String,
    /// unix seconds
    pub expires_at: i64,
    /// unix seconds
    pub last_seen_at: Option<i64>,
    pub user_agent: Option<String>,
    pub ip: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModelGroup {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub allowed_models: Vec<String>,
    pub created_at: i64,  // Unix timestamp
    pub updated_at: i64,  // Unix timestamp
}

impl ModelGroup {
    /// 检查模型是否在允许列表中（支持通配符 "*"）
    pub fn allows_model(&self, model: &str) -> bool {
        self.allowed_models.contains(&"*".to_string())
            || self.allowed_models.contains(&model.to_string())
    }
}

/// 迁移留档记录（downstream_access_migrations 的对外视图，无 secret）。
#[derive(Debug, Clone, Serialize)]
pub struct AccessMigrationRecord {
    pub downstream_id: String,
    pub classification: String,
    pub before_policy: Value,
    pub after_policy: Value,
    /// unix seconds
    pub created_at: i64,
    /// unix seconds
    pub resolved_at: Option<i64>,
    pub owner_user_id: Option<String>,
    pub mode: String,
    pub model_group_id: String,
    pub revision: i64,
}

/// 单条密钥的修复预览：现授权、待补组、组内容与指纹。
#[derive(Debug, Clone, Serialize)]
pub struct AccessMigrationPreviewItem {
    pub downstream_id: String,
    pub classification: String,
    pub owner_user_id: Option<String>,
    pub existing_user_groups: Vec<String>,
    pub candidate_group_ids: Vec<String>,
    pub group_models: std::collections::BTreeMap<String, Value>,
    pub revision: i64,
    pub fingerprint: String,
}

/// apply 提交项：识别密钥 + 待补组 + 预览时的 revision 与指纹。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessMigrationApplyItem {
    pub downstream_id: String,
    pub candidate_group_ids: Vec<String>,
    pub set_inherit: bool,
    pub expected_revision: i64,
    pub expected_fingerprint: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AccessMigrationItemResult {
    pub downstream_id: String,
    pub ok: bool,
    pub error: String,
    pub code: String,
}

fn migration_fingerprint(
    user_groups: &[String],
    group_models: &std::collections::BTreeMap<String, Value>,
    revision: i64,
) -> String {
    let mut hasher = Sha256::new();
    for group in user_groups {
        hasher.update(b"g:");
        hasher.update(group.as_bytes());
        hasher.update(b";");
    }
    for (group, models) in group_models {
        hasher.update(b"m:");
        hasher.update(group.as_bytes());
        hasher.update(b"=");
        hasher.update(models.to_string().as_bytes());
        hasher.update(b";");
    }
    hasher.update(b"r:");
    hasher.update(revision.to_string().as_bytes());
    format!("{:x}", hasher.finalize())
}

#[derive(Debug, thiserror::Error)]
pub enum PortalStoreError {
    #[error("portal record not found")]
    NotFound,
    #[error("portal conflict: {0}")]
    Conflict(String),
    #[error("portal access forbidden: {0}")]
    Forbidden(String),
    #[error("portal store failure: {0}")]
    Db(String),
}

impl From<tokio_postgres::Error> for PortalStoreError {
    fn from(error: tokio_postgres::Error) -> Self {
        PortalStoreError::Db(error.to_string())
    }
}

impl From<bb8::RunError<tokio_postgres::Error>> for PortalStoreError {
    fn from(error: bb8::RunError<tokio_postgres::Error>) -> Self {
        PortalStoreError::Db(error.to_string())
    }
}

#[derive(Clone)]
pub struct PortalStore {
    pool: Pool<Manager>,
}

impl PortalStore {
    pub fn from_pool(pool: Pool<Manager>) -> Self {
        Self { pool }
    }

    /// Get a database client (exposed for testing)
    pub async fn get_client(&self) -> Result<bb8::PooledConnection<'_, Manager>, PortalStoreError> {
        Ok(self.pool.get().await?)
    }

    /// (provider, subject) -> user row.  None when unbound.
    pub async fn find_user_by_identity(
        &self,
        provider: &str,
        subject: &str,
    ) -> Result<Option<PortalUser>, PortalStoreError> {
        let client = self.pool.get().await?;
        let result = client
            .query_opt(
                "SELECT u.id, u.email, u.display_name, u.username, u.disabled, \
                        EXTRACT(EPOCH FROM u.created_at)::bigint AS created_at, \
                        (CASE WHEN u.last_login_at IS NULL THEN NULL ELSE EXTRACT(EPOCH FROM u.last_login_at)::bigint END) AS last_login_at, \
                        i.provider, i.subject, \
                        (SELECT COUNT(*) FROM portal_user_downstreams b WHERE b.user_id = u.id) \
                 FROM portal_users u \
                 JOIN portal_identities i ON i.user_id = u.id \
                 WHERE i.provider = $1 AND i.subject = $2",
                &[&provider, &subject],
            )
            .await?;
        Ok(result.map(parse_user_row))
    }

    pub async fn find_user_by_id(&self, user_id: &str) -> Result<Option<PortalUser>, PortalStoreError> {
        let client = self.pool.get().await?;
        let result = client
            .query_opt(
                "SELECT u.id, u.email, u.display_name, u.username, u.disabled, \
                        EXTRACT(EPOCH FROM u.created_at)::bigint AS created_at, \
                        (CASE WHEN u.last_login_at IS NULL THEN NULL \
                              ELSE EXTRACT(EPOCH FROM u.last_login_at)::bigint END) AS last_login_at, \
                        (SELECT i.provider FROM portal_identities i \
                         WHERE i.user_id = u.id ORDER BY i.created_at LIMIT 1), \
                        (SELECT i.subject FROM portal_identities i \
                         WHERE i.user_id = u.id ORDER BY i.created_at LIMIT 1), \
                        (SELECT COUNT(*) FROM portal_user_downstreams b WHERE b.user_id = u.id) \
                 FROM portal_users u WHERE u.id = $1",
                &[&user_id],
            )
            .await?;
        Ok(result.map(parse_user_row))
    }

    /// Create user + first identity atomically (OIDC registration path).
    /// Unique-email or unique-(provider,subject) violations map to
    /// `PortalStoreError::Conflict`.

    /// Update the editable profile fields of a portal user.  `None` leaves a
    /// field untouched; `Some("")` clears it (stored as NULL).  The identity
    /// subject (uuid) is deliberately never updateable.
    pub async fn update_user_profile(
        &self,
        user_id: &str,
        display_name: Option<String>,
        username: Option<String>,
        email: Option<String>,
    ) -> Result<bool, PortalStoreError> {
        use tokio_postgres::types::ToSql;
        let client = self.pool.get().await?;
        let mut sets: Vec<String> = Vec::new();
        let mut params: Vec<Box<dyn ToSql + Sync + Send>> = vec![Box::new(user_id.to_string())];
        let mut push = |column: &str, value: Option<String>| {
            sets.push(format!("{column} = ${}", params.len() + 1));
            // Empty input clears the column to NULL; absent input keeps it.
            let normalized = value
                .map(|raw| raw.trim().to_string())
                .filter(|trimmed| !trimmed.is_empty());
            params.push(Box::new(normalized));
        };
        if let Some(value) = display_name {
            push("display_name", Some(value));
        }
        if let Some(value) = username {
            push("username", Some(value));
        }
        if let Some(value) = email {
            push("email", Some(value));
        }
        debug_assert!(!sets.is_empty(), "caller must provide at least one field");
        let sql = format!("UPDATE portal_users SET {} WHERE id = $1", sets.join(", "));
        let refs: Vec<&(dyn ToSql + Sync)> = params
            .iter()
            .map(|value| {
                let reference: &(dyn ToSql + Sync) = value.as_ref();
                reference
            })
            .collect();
        let affected = client
            .execute(&sql, &refs)
            .await
            .map_err(|error| classify_conflict(error, "email already in use"))?;
        Ok(affected == 1)
    }

    pub async fn create_user_with_identity(
        &self,
        email: &str,
        display_name: Option<&str>,
        username: Option<&str>,
        provider: &str,
        subject: &str,
    ) -> Result<PortalUser, PortalStoreError> {
        let mut client = self.pool.get().await?;
        let transaction = client.transaction().await?;
        let user_id = uuid::Uuid::new_v4().to_string();
        let insert_user = transaction
            .execute(
                "INSERT INTO portal_users (id, email, display_name, username) \
                 VALUES ($1, $2, $3, $4)",
                &[&user_id, &email, &display_name, &username],
            )
            .await
            .map_err(|error| classify_conflict(error, "email or identity already exists"))?;
        if insert_user != 1 {
            return Err(PortalStoreError::Db("user insert affected 0 rows".into()));
        }
        let insert_identity = transaction
            .execute(
                "INSERT INTO portal_identities (provider, subject, user_id) VALUES ($1, $2, $3)",
                &[&provider, &subject, &user_id],
            )
            .await
            .map_err(|error| classify_conflict(error, "email or identity already exists"))?;
        if insert_identity != 1 {
            return Err(PortalStoreError::Db("identity insert affected 0 rows".into()));
        }
        transaction.commit().await?;
        self.find_user_by_identity(provider, subject)
            .await
            .map(|user| user.expect("just-created user must be findable by identity"))
    }

    /// Bind another (provider, subject) to an existing user (bind flow).
    /// `Conflict` when the identity is already bound to a different user.
    pub async fn create_identity(
        &self,
        user_id: &str,
        provider: &str,
        subject: &str,
    ) -> Result<(), PortalStoreError> {
        let client = self.pool.get().await?;
        let inserted = client
            .execute(
                "INSERT INTO portal_identities (provider, subject, user_id) VALUES ($1, $2, $3) \
                 ON CONFLICT (provider, subject) DO NOTHING",
                &[&provider, &subject, &user_id],
            )
            .await?;
        if inserted == 1 {
            return Ok(());
        }
        let owner = client
            .query_opt(
                "SELECT user_id FROM portal_identities WHERE provider = $1 AND subject = $2",
                &[&provider, &subject],
            )
            .await?
            .map(|row| row.get::<_, String>(0));
        match owner {
            None => Ok(()),
            Some(owner) if owner == user_id => Ok(()),
            Some(_) => Err(PortalStoreError::Conflict(
                "identity is already bound to another user".into(),
            )),
        }
    }

    /// Look up a user by unique email (bind intent reuses an existing user).
    pub async fn portal_user_by_email(
        &self,
        email: &str,
    ) -> Result<Option<PortalUser>, PortalStoreError> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                "SELECT u.id, u.email, u.display_name, u.username, u.disabled,                         EXTRACT(EPOCH FROM u.created_at)::bigint,                         (CASE WHEN u.last_login_at IS NULL THEN NULL                          ELSE EXTRACT(EPOCH FROM u.last_login_at)::bigint END),                         (SELECT i.provider FROM portal_identities i                          WHERE i.user_id = u.id ORDER BY i.created_at LIMIT 1),                         (SELECT i.subject FROM portal_identities i                          WHERE i.user_id = u.id ORDER BY i.created_at LIMIT 1),                         (SELECT COUNT(*) FROM portal_user_downstreams b WHERE b.user_id = u.id)                  FROM portal_users u WHERE u.email = $1",
                &[&email],
            )
            .await?;
        Ok(row.map(parse_user_row))
    }

    pub async fn touch_last_login(&self, user_id: &str) -> Result<(), PortalStoreError> {
        let client = self.pool.get().await?;
        client
            .execute(
                "UPDATE portal_users SET last_login_at = NOW() WHERE id = $1",
                &[&user_id],
            )
            .await?;
        Ok(())
    }

    pub async fn list_downstream_bindings(
        &self, user_id: &str,
    ) -> Result<Vec<PortalDownstreamBinding>, PortalStoreError> {
        let client = self.pool.get().await?;
        let rows = client.query(
            "SELECT b.downstream_id,b.is_default,b.label,
                CASE WHEN p.mode='inherit' THEN '' ELSE COALESCE(p.model_group_id,'deny-all') END,
                COALESCE(p.mode,'deny')
             FROM portal_user_downstreams b LEFT JOIN downstream_access_policies p ON p.downstream_id=b.downstream_id
             WHERE b.user_id=$1 ORDER BY b.downstream_id", &[&user_id],
        ).await?;
        rows.iter().map(|row| {
            let group_id: String = row.get(3);
            let mode: AccessMode = serde_json::from_value(serde_json::json!(row.get::<_,String>(4)))
                .map_err(|e| PortalStoreError::Db(e.to_string()))?;
            Ok(PortalDownstreamBinding {
                downstream_id:row.get(0),is_default:row.get(1),label:row.get(2),model_group_id:group_id.clone(),
                model_access:ModelAccessSelection { mode,group_id:(mode==AccessMode::Group).then_some(group_id) },
            })
        }).collect()
    }

    pub async fn list_downstream_bindings_with_labels(
        &self, user_id: &str,
    ) -> Result<Vec<PortalDownstreamBindingWithLabel>, PortalStoreError> {
        let client = self.pool.get().await?;
        let rows = client.query(
            "SELECT b.downstream_id,b.is_default,COALESCE(b.label,d.name),
                CASE WHEN p.mode='inherit' THEN '' ELSE p.model_group_id END,
                CASE WHEN p.mode='group' THEN g.name ELSE NULL END,
                EXTRACT(EPOCH FROM COALESCE(b.created_at,NOW()))::bigint,
                (SELECT COUNT(*) FROM response_history r WHERE r.downstream_key_id=b.downstream_id),
                d.plaintext_key,p.mode,p.revision
             FROM portal_user_downstreams b JOIN downstream_access_policies p
                ON p.downstream_id=b.downstream_id AND p.owner_user_id=b.user_id
             JOIN downstreams d ON d.id=b.downstream_id
             LEFT JOIN model_groups g ON g.id=p.model_group_id
             WHERE b.user_id=$1 AND NOT EXISTS(
                SELECT 1 FROM portal_user_downstreams other WHERE other.downstream_id=b.downstream_id AND other.user_id<>b.user_id)
             ORDER BY b.is_default DESC,b.created_at DESC,b.downstream_id", &[&user_id],
        ).await?;
        rows.iter().map(|row| {
            let group_id: String = row.get(3);
            let mode: AccessMode = serde_json::from_value(serde_json::json!(row.get::<_,String>(8)))
                .map_err(|e| PortalStoreError::Db(e.to_string()))?;
            Ok(PortalDownstreamBindingWithLabel {
                downstream_id:row.get(0),is_default:row.get(1),label:row.get(2),model_group_id:group_id.clone(),
                model_group_name:row.get(4),created_at:row.get(5),usage_count:row.get(6),plaintext_key:row.get(7),
                model_access:ModelAccessSelection { mode,group_id:(mode==AccessMode::Group).then_some(group_id) },
                access_revision:row.get(9),
            })
        }).collect()
    }

    pub async fn count_user_keys(&self, user_id: &str) -> Result<i64, PortalStoreError> {
        let client = self.pool.get().await?;
        let row = client
            .query_one(
                "SELECT COUNT(*) FROM portal_user_downstreams WHERE user_id = $1",
                &[&user_id],
            )
            .await?;
        Ok(row.get(0))
    }

    /// Add a downstream binding with label and model_group_id.
    /// Sets is_default to FALSE initially, created_at to NOW().
    /// ON CONFLICT DO NOTHING makes this idempotent.
    pub async fn add_downstream_binding_with_label(
        &self,user_id:&str,downstream_id:&str,label:Option<&str>,model_group_id:Option<&str>,
    ) -> Result<(),PortalStoreError> {
        self.mutate_access(&AccessMutation::Bind {
            downstream_id:downstream_id.into(),user_id:user_id.into(),is_default:None,
            label:label.map(str::to_owned),selection:model_group_id.map(ModelAccessSelection::from_group_id),
            grant_existing:false,
        }).await
    }

    /// Update the label and model_group_id for an existing downstream binding.
    /// Does not check if the row exists - caller should handle missing rows.
    /// Supports NULL values to clear the label/model_group.
    pub async fn update_downstream_label(
        &self,user_id:&str,downstream_id:&str,label:Option<&str>,_model_group_id:Option<&str>,
    ) -> Result<(),PortalStoreError> {
        self.mutate_access(&AccessMutation::SetLabel {
            downstream_id:downstream_id.into(),user_id:user_id.into(),label:label.map(str::to_owned),
        }).await
    }

    /// Update only the model_group_id of an existing downstream binding,
    /// preserving the label.  Returns NotFound when the binding does not
    /// exist for this user.
    pub async fn update_downstream_model_group(
        &self,user_id:&str,downstream_id:&str,model_group_id:&str,
    ) -> Result<(),PortalStoreError> {
        self.mutate_access(&AccessMutation::Update {
            downstream_id:downstream_id.into(),selection:ModelAccessSelection::from_group_id(model_group_id),
            actor_user_id:Some(user_id.into()),
        }).await
    }

    /// Safe delete: only removes if non-default AND no usage history.
    /// Returns Ok(true) if deleted, Ok(false) if rejected (default or has usage).
    /// Must run in a transaction to ensure consistency.
    /// Set a downstream binding as the default key.
    /// Clears all other defaults for this user in a transaction to ensure uniqueness.
    /// Silently succeeds even if downstream_id doesn't exist (no rows updated).
    pub async fn set_default_key(
        &self,user_id:&str,downstream_id:&str,
    ) -> Result<(),PortalStoreError> {
        self.mutate_access(&AccessMutation::SetDefault { downstream_id:downstream_id.into(),user_id:user_id.into() }).await
    }

    /// Add a binding; setting `is_default` demotes every other row.  Returns
    /// `NotFound` when the user does not exist.
    pub async fn add_downstream_binding(
        &self,user_id:&str,downstream_id:&str,is_default:bool,
    ) -> Result<(),PortalStoreError> {
        self.mutate_access(&AccessMutation::Bind {
            downstream_id:downstream_id.into(),user_id:user_id.into(),is_default:Some(is_default),
            label:None,selection:None,grant_existing:false,
        }).await
    }

    /// Add (or update) a binding, optionally pinning the model group.
    /// Same semantics as `add_downstream_binding` plus `model_group_id`
    /// (None keeps the previous group / default 'basic' on insert).
    pub async fn upsert_downstream_binding_with_group(
        &self,user_id:&str,downstream_id:&str,is_default:bool,model_group_id:Option<&str>,
    ) -> Result<(),PortalStoreError> {
        self.mutate_access(&AccessMutation::Bind {
            downstream_id:downstream_id.into(),user_id:user_id.into(),is_default:Some(is_default),
            label:None,selection:model_group_id.map(ModelAccessSelection::from_group_id),grant_existing:false,
        }).await
    }

    /// Remove a binding; when it was the default and bindings remain, another
    /// row is promoted so the user still has exactly one default.
    pub async fn remove_downstream_binding(
        &self,user_id:&str,downstream_id:&str,
    ) -> Result<(),PortalStoreError> {
        self.mutate_access(&AccessMutation::Unbind { downstream_id:downstream_id.into(),user_id:user_id.into() }).await
    }

    pub async fn default_downstream(
        &self,
        user_id: &str,
    ) -> Result<Option<String>, PortalStoreError> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                "SELECT downstream_id FROM portal_user_downstreams \
                 WHERE user_id = $1 AND is_default",
                &[&user_id],
            )
            .await?;
        Ok(row.map(|row| row.get(0)))
    }

    /// Return the durable owner, never an arbitrary legacy binding.
    pub async fn find_user_id_by_downstream(
        &self,
        downstream_id: &str,
    ) -> Result<Option<String>, PortalStoreError> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                "SELECT owner_user_id FROM downstream_access_policies WHERE downstream_id = $1",
                &[&downstream_id],
            )
            .await?;
        Ok(row.and_then(|row| row.get(0)))
    }

    /// Lazily provision a portal account for a Bearer-authenticated downstream
    /// (工号+密钥 login).  The downstream id is the login identity, so the
    /// account is created on first use and the downstream is bound as its
    /// default key.  Idempotent: returns the existing owner when the
    /// downstream is already bound.
    pub async fn ensure_user_for_downstream(
        &self, downstream_id: &str, display_name: Option<&str>,
    ) -> Result<String, PortalStoreError> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        super::model_access_store::lock_access_writes(&tx).await?;
        let row = tx.query_opt(
            "SELECT subject_kind,owner_user_id FROM downstream_access_policies WHERE downstream_id=$1 FOR UPDATE",
            &[&downstream_id],
        ).await?.ok_or(PortalStoreError::NotFound)?;
        let owner: Option<String> = row.get(1);
        if let Some(owner) = owner {
            let user = tx.query_one("SELECT disabled FROM portal_users WHERE id=$1", &[&owner]).await?;
            if user.get::<_,bool>(0) { return Err(PortalStoreError::Forbidden("user_disabled".into())); }
            tx.commit().await?;
            return Ok(owner);
        }
        if row.get::<_,String>(0) == "portal" {
            return Err(PortalStoreError::Forbidden("owner_missing".into()));
        }
        let generated_id = uuid::Uuid::new_v4().to_string();
        let email = format!("{downstream_id}@downstream.local");
        tx.execute(
            "INSERT INTO portal_users(id,email,display_name,username) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING",
            &[&generated_id,&email,&display_name,&downstream_id],
        ).await?;
        let user_id: String = tx.query_one("SELECT id FROM portal_users WHERE email=$1", &[&email]).await?.get(0);
        super::model_access_store::apply_access_mutation(&tx, &AccessMutation::Bind {
            downstream_id:downstream_id.into(),user_id:user_id.clone(),is_default:Some(true),
            label:None,selection:None,grant_existing:true,
        }).await?;
        tx.commit().await?;
        Ok(user_id)
    }

    pub async fn create_session(
        &self,
        sid_hash: &str,
        user_id: &str,
        expires_at_unix: i64,
        user_agent: Option<&str>,
        ip: Option<&str>,
    ) -> Result<(), PortalStoreError> {
        let client = self.pool.get().await?;
        client
            .execute(
                "INSERT INTO portal_sessions (sid, user_id, expires_at, user_agent, ip) \
                 VALUES ($1, $2, to_timestamp($3::bigint), $4, $5)",
                &[&sid_hash, &user_id, &expires_at_unix, &user_agent, &ip],
            )
            .await?;
        Ok(())
    }

    /// Look up a live session whose user is not disabled.  Expired sessions
    /// and disabled users both yield `None`.
    pub async fn find_session(
        &self,
        sid_hash: &str,
    ) -> Result<Option<PortalSession>, PortalStoreError> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                "SELECT s.sid, s.user_id, EXTRACT(EPOCH FROM s.expires_at)::bigint, \
                        COALESCE(EXTRACT(EPOCH FROM s.last_seen_at)::bigint, 0), \
                        s.user_agent, s.ip \
                 FROM portal_sessions s \
                 JOIN portal_users u ON u.id = s.user_id \
                 WHERE s.sid = $1 AND s.expires_at > NOW() AND NOT u.disabled",
                &[&sid_hash],
            )
            .await?;
        Ok(row.map(|row| PortalSession {
            sid: row.get(0),
            user_id: row.get(1),
            expires_at: row.get(2),
            last_seen_at: Some(row.get(3)).filter(|epoch| *epoch > 0),
            user_agent: row.get(4),
            ip: row.get(5),
        }))
    }

    pub async fn delete_session(&self, sid_hash: &str) -> Result<(), PortalStoreError> {
        let client = self.pool.get().await?;
        client
            .execute("DELETE FROM portal_sessions WHERE sid = $1", &[&sid_hash])
            .await?;
        Ok(())
    }

    pub async fn touch_session(&self, sid_hash: &str) -> Result<(), PortalStoreError> {
        let client = self.pool.get().await?;
        client
            .execute(
                "UPDATE portal_sessions SET last_seen_at = NOW() \
                 WHERE sid = $1 AND expires_at > NOW()",
                &[&sid_hash],
            )
            .await?;
        Ok(())
    }

    /// Enable/disable.  Disabling purges every session of the user **in the
    /// same transaction** (design §4.4).  `Ok(false)` when the user is absent.
    pub async fn set_user_disabled(
        &self,
        user_id: &str,
        disabled: bool,
    ) -> Result<bool, PortalStoreError> {
        let mut client = self.pool.get().await?;
        let transaction = client.transaction().await?;
        let affected = transaction
            .execute(
                "UPDATE portal_users SET disabled = $2 WHERE id = $1",
                &[&user_id, &disabled],
            )
            .await?;
        if affected == 0 {
            return Ok(false);
        }
        if disabled {
            transaction
                .execute(
                    "DELETE FROM portal_sessions WHERE user_id = $1",
                    &[&user_id],
                )
                .await?;
        }
        transaction.commit().await?;
        Ok(true)
    }

    /// Keyword search over email/display_name/username with paging; returns
    /// `(total, page)`.
    pub async fn list_users(
        &self,
        keyword: &str,
        limit: i64,
        offset: i64,
    ) -> Result<(i64, Vec<PortalUser>), PortalStoreError> {
        let client = self.pool.get().await?;
        let filter = format!(
            "($1 = '' OR u.email ILIKE '%' || $1 || '%' \
              OR COALESCE(u.display_name, '') ILIKE '%' || $1 || '%' \
              OR COALESCE(u.username, '') ILIKE '%' || $1 || '%')"
        );
        let total: i64 = client
            .query_one(
                &format!("SELECT COUNT(*) FROM portal_users u WHERE {filter}"),
                &[&keyword],
            )
            .await?
            .get(0);
        let rows = client
            .query(
                &format!(
                    "SELECT u.id, u.email, u.display_name, u.username, u.disabled, \
                            EXTRACT(EPOCH FROM u.created_at)::bigint, \
                            (CASE WHEN u.last_login_at IS NULL THEN NULL \
                             ELSE EXTRACT(EPOCH FROM u.last_login_at)::bigint END), \
                            (SELECT i.provider FROM portal_identities i \
                             WHERE i.user_id = u.id ORDER BY i.created_at LIMIT 1), \
                            (SELECT i.subject FROM portal_identities i \
                             WHERE i.user_id = u.id ORDER BY i.created_at LIMIT 1), \
                            (SELECT COUNT(*) FROM portal_user_downstreams b WHERE b.user_id = u.id) \
                     FROM portal_users u WHERE {filter} \
                     ORDER BY u.created_at DESC LIMIT {limit} OFFSET {offset}"
                ),
                &[&keyword],
            )
            .await?;
        Ok((total, rows.into_iter().map(parse_user_row).collect()))
    }

    /// List all model groups with their allowed models.
    /// Returns groups ordered by id.
    pub async fn list_model_groups(&self) -> Result<Vec<ModelGroup>, PortalStoreError> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT id, name, description, allowed_models::text, \
                        EXTRACT(EPOCH FROM created_at)::bigint, \
                        EXTRACT(EPOCH FROM updated_at)::bigint \
                 FROM model_groups \
                 ORDER BY id",
                &[],
            )
            .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let allowed_models_json: String = row.get(3);
                let allowed_models: Vec<String> = serde_json::from_str(&allowed_models_json)
                    .unwrap_or_default();

                ModelGroup {
                    id: row.get(0),
                    name: row.get(1),
                    description: row.get(2),
                    allowed_models,
                    created_at: row.get(4),
                    updated_at: row.get(5),
                }
            })
            .collect())
    }

    /// Get a single model group by id.
    pub async fn get_model_group(&self, id: &str) -> Result<ModelGroup, PortalStoreError> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                "SELECT id, name, description, allowed_models::text, \
                        EXTRACT(EPOCH FROM created_at)::bigint, \
                        EXTRACT(EPOCH FROM updated_at)::bigint \
                 FROM model_groups \
                 WHERE id = $1",
                &[&id],
            )
            .await?
            .ok_or(PortalStoreError::NotFound)?;

        let allowed_models_json: String = row.get(3);
        let allowed_models: Vec<String> = serde_json::from_str(&allowed_models_json)
            .unwrap_or_default();

        Ok(ModelGroup {
            id: row.get(0),
            name: row.get(1),
            description: row.get(2),
            allowed_models,
            created_at: row.get(4),
            updated_at: row.get(5),
        })
    }

    /// Create a new model group.  The id must match `^[a-z0-9-]+$` (the same
    /// CHECK the schema enforces); duplicates surface as `Conflict`.
    pub async fn create_model_group(&self, group: &ModelGroup) -> Result<(), PortalStoreError> {
        let valid_id = !group.id.is_empty()
            && group
                .id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !valid_id {
            return Err(PortalStoreError::Conflict(
                "invalid group id format".to_string(),
            ));
        }
        let client = self.pool.get().await?;
        let allowed_models_json = serde_json::to_value(&group.allowed_models)
            .map_err(|e| PortalStoreError::Db(e.to_string()))?;

        // Bind `serde_json::Value` (JSONB-aware via with-serde_json-1); a plain
        // String does not implement accepts(jsonb).
        client
            .execute(
                "INSERT INTO model_groups (id, name, description, allowed_models) \
                 VALUES ($1, $2, $3, $4)",
                &[
                    &group.id,
                    &group.name,
                    &group.description.as_deref(),
                    &allowed_models_json,
                ],
            )
            .await
            .map(|_| ())
            .map_err(|error| classify_conflict(error, "group id already exists"))
    }

    /// Update an existing model group.
    /// T15 哨兵组：all 是通配符语义的事实源（必须保持 ["*"]），deny-all 是
    /// FK NOT NULL DEFAULT 的引用目标（必须保持拒绝全部），二者不可编辑。
    fn is_sentinel_group(id: &str) -> bool {
        matches!(id, "all" | "deny-all")
    }

    pub async fn update_model_group(
        &self,
        id: &str,
        name: &str,
        description: Option<&str>,
        allowed_models: Vec<String>,
    ) -> Result<(), PortalStoreError> {
        if Self::is_sentinel_group(id) {
            return Err(PortalStoreError::Conflict(
                format!("cannot modify builtin sentinel group {id}"),
            ));
        }
        let client = self.pool.get().await?;
        let allowed_models_json = serde_json::to_value(&allowed_models)
            .map_err(|e| PortalStoreError::Db(e.to_string()))?;

        // description is already Option<&str>, which postgres-types handles correctly
        let rows = client
            .execute(
                "UPDATE model_groups \
                 SET name = $2, description = $3, allowed_models = $4, updated_at = NOW() \
                 WHERE id = $1",
                &[&id, &name, &description, &allowed_models_json],
            )
            .await?;
        if rows == 0 {
            return Err(PortalStoreError::NotFound);
        }
        Ok(())
    }

    /// Delete a model group.  All builtin groups are protected (basic/premium
    /// 业务组、all/deny-all T15 哨兵组)；删除其他组时下游行经外键
    /// `ON DELETE SET DEFAULT` 落 deny-all（T15 起不再是 basic）。
    pub async fn delete_model_group(&self, id: &str) -> Result<(), PortalStoreError> {
        if matches!(id, "basic" | "premium" | "all" | "deny-all") {
            return Err(PortalStoreError::Conflict(format!(
                "cannot delete builtin model group {id}"
            )));
        }
        let client = self.pool.get().await?;
        let rows = client
            .execute("DELETE FROM model_groups WHERE id = $1", &[&id])
            .await?;
        if rows == 0 {
            return Err(PortalStoreError::NotFound);
        }
        Ok(())
    }

    /// 列出用户有权访问的模型分组（basic 默认可见）。
    pub async fn list_user_accessible_model_groups(
        &self,
        user_id: &str,
    ) -> Result<Vec<ModelGroup>, PortalStoreError> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT mg.id, mg.name, mg.description, mg.allowed_models::text, \
                        EXTRACT(EPOCH FROM mg.created_at)::bigint, \
                        EXTRACT(EPOCH FROM mg.updated_at)::bigint \
                 FROM model_groups mg \
                 WHERE mg.id = 'basic' \
                    OR EXISTS (
                        SELECT 1 FROM portal_user_model_groups pumg \
                        WHERE pumg.user_id = $1 AND pumg.model_group_id = mg.id
                    ) \
                 ORDER BY mg.id",
                &[&user_id],
            )
            .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let allowed_models_json: String = row.get(3);
                let allowed_models: Vec<String> = serde_json::from_str(&allowed_models_json)
                    .unwrap_or_default();

                ModelGroup {
                    id: row.get(0),
                    name: row.get(1),
                    description: row.get(2),
                    allowed_models,
                    created_at: row.get(4),
                    updated_at: row.get(5),
                }
            })
            .collect())
    }

    /// 批量查询多个用户被显式授权的模型分组 id（不含 basic，basic 恒可见）。
    /// 返回 user_id -> 已授权分组 id 列表（已排序）。
    pub async fn list_users_model_group_ids(
        &self,
        user_ids: &[String],
    ) -> Result<std::collections::HashMap<String, Vec<String>>, PortalStoreError> {
        let mut result = std::collections::HashMap::new();
        if user_ids.is_empty() {
            return Ok(result);
        }
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT user_id, model_group_id                  FROM portal_user_model_groups                  WHERE user_id = ANY($1)                  ORDER BY model_group_id",
                &[&user_ids],
            )
            .await?;
        for row in rows {
            let user_id: String = row.get(0);
            let group_id: String = row.get(1);
            result.entry(user_id).or_insert_with(Vec::new).push(group_id);
        }
        Ok(result)
    }

    /// 检查用户是否有权访问指定分组。
    pub async fn user_can_access_model_group(
        &self,
        user_id: &str,
        model_group_id: &str,
    ) -> Result<bool, PortalStoreError> {
        if model_group_id == "basic" {
            return Ok(true);
        }
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                "SELECT 1 FROM portal_user_model_groups \
                 WHERE user_id = $1 AND model_group_id = $2",
                &[&user_id, &model_group_id],
            )
            .await?;
        Ok(row.is_some())
    }

    /// 授予用户对分组的访问权限（管理员操作）。
    pub async fn grant_user_model_group(
        &self,
        user_id: &str,
        model_group_id: &str,
        granted_by: Option<&str>,
    ) -> Result<(), PortalStoreError> {
        let client = self.pool.get().await?;
        client
            .execute(
                "INSERT INTO portal_user_model_groups (user_id, model_group_id, granted_by) \
                 VALUES ($1, $2, $3) \
                 ON CONFLICT (user_id, model_group_id) DO NOTHING",
                &[&user_id, &model_group_id, &granted_by],
            )
            .await?;
        Ok(())
    }

    /// 撤销用户对分组的访问权限（管理员操作）。
    pub async fn revoke_user_model_group(
        &self,
        user_id: &str,
        model_group_id: &str,
    ) -> Result<(), PortalStoreError> {
        if model_group_id == "basic" {
            return Err(PortalStoreError::Forbidden(
                "cannot revoke basic group access".to_string(),
            ));
        }
        let client = self.pool.get().await?;
        client
            .execute(
                "DELETE FROM portal_user_model_groups \
                 WHERE user_id = $1 AND model_group_id = $2",
                &[&user_id, &model_group_id],
            )
            .await?;
        Ok(())
    }

    /// 原子替换用户的模型组授权：basic + 显式目标集合整体替换。
    /// 组缺失或任一步失败时整个事务回滚，不产生半套授权。
    pub async fn replace_user_model_groups(
        &self,
        user_id: &str,
        target_ids: &[String],
    ) -> Result<(), PortalStoreError> {
        let mut client = self.get_client().await?;
        let tx = client.transaction().await?;
        let user_row = tx
            .query_opt("SELECT id FROM portal_users WHERE id = $1 FOR UPDATE", &[&user_id])
            .await?
            .ok_or(PortalStoreError::NotFound)?;
        drop(user_row);

        let mut target: Vec<String> = target_ids.iter().cloned().collect();
        target.sort();
        target.dedup();
        if !target.iter().any(|id| id == "basic") {
            target.push("basic".to_string());
            target.sort();
        }

        // 校验目标组都存在（basic 恒存在）。
        for group_id in &target {
            if group_id == "basic" {
                continue;
            }
            let row = tx
                .query_opt("SELECT id FROM model_groups WHERE id = $1 FOR SHARE", &[group_id])
                .await?
                .ok_or(PortalStoreError::NotFound)?;
            drop(row);
        }

        tx.execute(
            "DELETE FROM portal_user_model_groups WHERE user_id = $1 AND model_group_id <> 'basic'",
            &[&user_id],
        )
        .await?;
        for group_id in &target {
            if group_id == "basic" {
                continue;
            }
            tx.execute(
                "INSERT INTO portal_user_model_groups (user_id, model_group_id, granted_by) \
                 VALUES ($1, $2, 'admin') ON CONFLICT (user_id, model_group_id) DO NOTHING",
                &[&user_id, group_id],
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Get allowed models for a specific downstream key.
    /// Returns the allowed_models list from the key's model_group, or empty vec if no group assigned.
    pub async fn get_key_allowed_models(
        &self, downstream_id: &str,
    ) -> Result<Vec<String>, PortalStoreError> {
        let catalog = super::ModelCatalog::new(&[], &super::model_identity::ModelAliasRegistry::default(), true);
        let mut access = self.resolve_key_access(&[downstream_id.into()], &catalog).await?;
        Ok(access.remove(downstream_id).ok_or(PortalStoreError::NotFound)?.allowed.legacy_projection())
    }

    /// 待处理迁移记录：downstream_access_migrations 中尚未解决的行，
    /// 附当前策略派生状态。不含任何 secret/hash。
    pub async fn access_migration_records(
        &self,
    ) -> Result<Vec<AccessMigrationRecord>, PortalStoreError> {
        let client = self.get_client().await?;
        let rows = client
            .query(
                "SELECT m.downstream_id, m.classification, m.before_policy, m.after_policy,
                        EXTRACT(EPOCH FROM m.created_at)::bigint,
                        EXTRACT(EPOCH FROM m.resolved_at)::bigint,
                        p.owner_user_id, p.mode, p.model_group_id, p.revision
                 FROM downstream_access_migrations m
                 JOIN downstream_access_policies p ON p.downstream_id = m.downstream_id
                 WHERE m.migration_version = 1
                 ORDER BY m.downstream_id",
                &[],
            )
            .await?;
        rows.iter()
            .map(|row| {
                Ok(AccessMigrationRecord {
                    downstream_id: row.get(0),
                    classification: row.get(1),
                    before_policy: row.get(2),
                    after_policy: row.get(3),
                    created_at: row.get(4),
                    resolved_at: row.get(5),
                    owner_user_id: row.get(6),
                    mode: row.get(7),
                    model_group_id: row.get(8),
                    revision: row.get(9),
                })
            })
            .collect()
    }

    /// 迁移修复预览：对指定待处理密钥，返回 owner 的现有授权、待补旧绑定组、
    /// 各组内容以及一个指纹。指纹由用户授权集合 + 候选组内容 + 策略 revision
    /// 计算，apply 时在事务内重新计算并比对（任何变化返回 409）。
    pub async fn access_migration_preview(
        &self,
        downstream_ids: &[String],
    ) -> Result<Vec<AccessMigrationPreviewItem>, PortalStoreError> {
        if downstream_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut client = self.get_client().await?;
        let tx = client
            .build_transaction()
            .isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
            .read_only(true)
            .start()
            .await?;
        let rows = tx
            .query(
                "SELECT m.downstream_id, m.classification, m.before_policy,
                        p.owner_user_id, p.revision
                 FROM downstream_access_migrations m
                 JOIN downstream_access_policies p ON p.downstream_id = m.downstream_id
                 WHERE m.downstream_id = ANY($1) AND m.migration_version = 1
                 ORDER BY m.downstream_id",
                &[&downstream_ids],
            )
            .await?;
        let mut result = Vec::new();
        for row in rows {
            let downstream_id: String = row.get(0);
            let classification: String = row.get(1);
            let before: Value = row.get(2);
            let owner_user_id: Option<String> = row.get(3);
            let revision: i64 = row.get(4);
            let binding_group_ids: Vec<String> = before
                .get("binding_group_ids")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            let mut existing_user_groups: Vec<String> = Vec::new();
            let mut group_models = std::collections::BTreeMap::new();
            if let Some(user_id) = &owner_user_id {
                for grant in tx
                    .query(
                        "SELECT model_group_id FROM portal_user_model_groups \
                         WHERE user_id = $1 ORDER BY model_group_id",
                        &[user_id],
                    )
                    .await?
                {
                    existing_user_groups.push(grant.get(0));
                }
            }
            for group_id in &binding_group_ids {
                let value: Value = tx
                    .query_one(
                        "SELECT allowed_models FROM model_groups WHERE id = $1",
                        &[group_id],
                    )
                    .await?
                    .get(0);
                group_models.insert(group_id.clone(), value);
            }
            let fingerprint = migration_fingerprint(
                &existing_user_groups,
                &group_models,
                revision,
            );
            result.push(AccessMigrationPreviewItem {
                downstream_id,
                classification,
                owner_user_id,
                existing_user_groups,
                candidate_group_ids: binding_group_ids,
                group_models,
                revision,
                fingerprint,
            });
        }
        tx.commit().await?;
        Ok(result)
    }

    /// 应用选定的迁移修复（设计 4.2 / P10）：每把密钥一个事务，
    /// 锁定策略行后重新核对指纹与 revision；变化返回 409（Conflict），
    /// 归属冲突（ownership_conflict/orphan）或已解决记录直接拒绝。
    /// 成功后补授权、置显式 inherit、标记 resolved_at。
    pub async fn apply_access_migration(
        &self,
        items: &[AccessMigrationApplyItem],
    ) -> Result<Vec<AccessMigrationItemResult>, PortalStoreError> {
        let mut results = Vec::new();
        for item in items {
            let outcome = self
                .apply_access_migration_one(item)
                .await
                .unwrap_or_else(|error| {
                    AccessMigrationItemResult {
                        downstream_id: item.downstream_id.clone(),
                        ok: false,
                        error: error.to_string(),
                        code: match error {
                            PortalStoreError::Conflict(_) => "conflict",
                            PortalStoreError::Forbidden(_) => "forbidden",
                            PortalStoreError::NotFound => "not_found",
                            PortalStoreError::Db(_) => "db_error",
                        }
                        .to_owned(),
                    }
                });
            results.push(outcome);
        }
        Ok(results)
    }

    async fn apply_access_migration_one(
        &self,
        item: &AccessMigrationApplyItem,
    ) -> Result<AccessMigrationItemResult, PortalStoreError> {
        let mut client = self.get_client().await?;
        let tx = client.transaction().await?;
        super::model_access_store::lock_access_writes(&tx).await?;
        let migration = tx
            .query_opt(
                "SELECT classification, before_policy, resolved_at
                 FROM downstream_access_migrations
                 WHERE downstream_id = $1 AND migration_version = 1 FOR UPDATE",
                &[&item.downstream_id],
            )
            .await?
            .ok_or(PortalStoreError::NotFound)?;
        let classification: String = migration.get(0);
        let before: Value = migration.get(1);
        let resolved_at: Option<chrono::DateTime<chrono::Utc>> = migration.get(2);
        if resolved_at.is_some() {
            return Err(PortalStoreError::Conflict(
                "migration record already resolved".into(),
            ));
        }
        if !matches!(
            classification.as_str(),
            "preserved" | "review_required"
        ) {
            return Err(PortalStoreError::Conflict(
                "ownership is unresolved (conflict/orphan); create a replacement key instead".into(),
            ));
        }
        let policy = tx
            .query_opt(
                "SELECT owner_user_id, revision FROM downstream_access_policies \
                 WHERE downstream_id = $1 FOR UPDATE",
                &[&item.downstream_id],
            )
            .await?
            .ok_or(PortalStoreError::NotFound)?;
        let owner_user_id: Option<String> = policy.get(0);
        let revision: i64 = policy.get(1);
        if revision != item.expected_revision {
            return Err(PortalStoreError::Conflict(
                "access policy revision changed since preview".into(),
            ));
        }
        let Some(user_id) = owner_user_id else {
            return Err(PortalStoreError::Forbidden("owner_missing".into()));
        };
        let binding_group_ids: Vec<String> = before
            .get("binding_group_ids")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        for group_id in &item.candidate_group_ids {
            if !binding_group_ids.contains(group_id) {
                return Err(PortalStoreError::Conflict(format!(
                    "group '{group_id}' is not a migration candidate for this key"
                )));
            }
            if matches!(group_id.as_str(), "deny-all" | "all") {
                return Err(PortalStoreError::Conflict(format!(
                    "group '{group_id}' cannot be applied as a migration grant"
                )));
            }
        }
        // 重新计算指纹：现有授权 + 候选组内容 + revision。
        let mut existing_user_groups: Vec<String> = Vec::new();
        for grant in tx
            .query(
                "SELECT model_group_id FROM portal_user_model_groups \
                 WHERE user_id = $1 ORDER BY model_group_id",
                &[&user_id],
            )
            .await?
        {
            existing_user_groups.push(grant.get(0));
        }
        let mut group_models = std::collections::BTreeMap::new();
        for group_id in &item.candidate_group_ids {
            let value: Value = tx
                .query_one(
                    "SELECT allowed_models FROM model_groups WHERE id = $1 FOR SHARE",
                    &[group_id],
                )
                .await?
                .get(0);
            group_models.insert(group_id.clone(), value);
        }
        let fingerprint = migration_fingerprint(
            &existing_user_groups,
            &group_models,
            revision,
        );
        if fingerprint != item.expected_fingerprint {
            return Err(PortalStoreError::Conflict(
                "preview is stale; user grants, group content or policy changed".into(),
            ));
        }
        for group_id in &item.candidate_group_ids {
            tx.execute(
                "INSERT INTO portal_user_model_groups (user_id, model_group_id, granted_by) \
                 VALUES ($1, $2, 'access-migration-apply') ON CONFLICT DO NOTHING",
                &[&user_id, group_id],
            )
            .await?;
        }
        if item.set_inherit {
            tx.execute(
                "UPDATE downstream_access_policies SET mode = 'inherit', \
                 model_group_id = 'deny-all', revision = revision + 1 \
                 WHERE downstream_id = $1",
                &[&item.downstream_id],
            )
            .await?;
        }
        tx.execute(
            "UPDATE downstream_access_migrations SET resolved_at = NOW() \
             WHERE downstream_id = $1 AND migration_version = 1",
            &[&item.downstream_id],
        )
        .await?;
        tx.commit().await?;
        Ok(AccessMigrationItemResult {
            downstream_id: item.downstream_id.clone(),
            ok: true,
            error: String::new(),
            code: "applied".to_owned(),
        })
    }
}

fn parse_user_row(row: tokio_postgres::Row) -> PortalUser {
    PortalUser {
        id: row.get(0),
        email: row.get(1),
        display_name: row.get(2),
        username: row.get(3),
        disabled: row.get(4),
        created_at: row.get(5),
        last_login_at: row.get(6),
        provider: row.get(7),
        subject: row.get(8),
        binding_count: row.get(9),
    }
}

// Implement the trait needed by DownstreamConfig::get_allowed_models()
impl crate::state::types::HasGetModelGroupModels for PortalStore {
    async fn get_model_group_models(&self, id: &str) -> Result<Vec<String>, String> {
        match self.get_model_group(id).await {
            Ok(group) => Ok(group.allowed_models),
            Err(e) => Err(format!("Failed to get model group: {}", e)),
        }
    }
}

fn classify_conflict(error: tokio_postgres::Error, message: &str) -> PortalStoreError {
    if error
        .as_db_error()
        .map(|db| db.code().code() == "23505")
        .unwrap_or(false)
    {
        PortalStoreError::Conflict(message.to_string())
    } else {
        PortalStoreError::Db(error.to_string())
    }
}

/// One in-flight OIDC login state (design §4.1 step 1, T3).  Kept in process
/// memory (multi-instance deployments share a sticky reconciliation via the
/// admin flow), keyed by the **raw** state value the client echoes back.
#[derive(Debug, Clone)]
pub struct PortalOidcHandshake {
    pub code_verifier: Option<String>,
    pub downstream_id: Option<String>,
    /// unix seconds; use `state::unix_seconds()` when checking
    pub expires_at_unix: i64,
}
