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
}

impl PortalDownstreamBinding {
    /// 获取 label，现有数据返回默认值
    pub fn label(&self) -> &str {
        self.label.as_deref().unwrap_or("Default Key")
    }
}

#[derive(Debug, Clone)]
pub struct PortalDownstreamBindingWithLabel {
    pub downstream_id: String,
    pub is_default: bool,
    pub label: String,  // 前端总是收到非空 label
    pub model_group_id: String,  // 模型分组
    pub created_at: i64,  // Unix timestamp
    pub usage_count: i64,  // 使用次数（从 response_history 统计）
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

#[derive(Debug, thiserror::Error)]
pub enum PortalStoreError {
    #[error("portal record not found")]
    NotFound,
    #[error("portal conflict: {0}")]
    Conflict(String),
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

    /// Create user + first identity atomically (OIDC registration path).
    /// Unique-email or unique-(provider,subject) violations map to
    /// `PortalStoreError::Conflict`.
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
        &self,
        user_id: &str,
    ) -> Result<Vec<PortalDownstreamBinding>, PortalStoreError> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT downstream_id, is_default, label, model_group_id FROM portal_user_downstreams \
                 WHERE user_id = $1 ORDER BY downstream_id",
                &[&user_id],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|row| PortalDownstreamBinding {
                downstream_id: row.get(0),
                is_default: row.get(1),
                label: row.get(2),
                model_group_id: row.get(3),
            })
            .collect())
    }

    pub async fn list_downstream_bindings_with_labels(
        &self,
        user_id: &str,
    ) -> Result<Vec<PortalDownstreamBindingWithLabel>, PortalStoreError> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT d.downstream_id, d.is_default, \
                        COALESCE(d.label, 'Default Key') AS label, \
                        d.model_group_id, \
                        EXTRACT(EPOCH FROM COALESCE(d.created_at, NOW()))::bigint AS created_at, \
                        COALESCE(COUNT(r.id), 0) AS usage_count \
                 FROM portal_user_downstreams d \
                 LEFT JOIN response_history r ON d.downstream_id = r.downstream_key_id \
                 WHERE d.user_id = $1 \
                 GROUP BY d.downstream_id, d.is_default, d.label, d.model_group_id, d.created_at \
                 ORDER BY d.is_default DESC, d.created_at DESC",
                &[&user_id],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|row| PortalDownstreamBindingWithLabel {
                downstream_id: row.get(0),
                is_default: row.get(1),
                label: row.get(2),
                model_group_id: row.get(3),
                created_at: row.get(4),
                usage_count: row.get(5),
            })
            .collect())
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
        &self,
        user_id: &str,
        downstream_id: &str,
        label: Option<&str>,
        model_group_id: Option<&str>,
    ) -> Result<(), PortalStoreError> {
        let client = self.pool.get().await?;
        client
            .execute(
                "INSERT INTO portal_user_downstreams \
                   (user_id, downstream_id, label, model_group_id, is_default, created_at) \
                 VALUES ($1, $2, $3, $4, FALSE, NOW()) \
                 ON CONFLICT (user_id, downstream_id) DO NOTHING",
                &[&user_id, &downstream_id, &label, &model_group_id],
            )
            .await?;
        Ok(())
    }

    /// Update the label and model_group_id for an existing downstream binding.
    /// Does not check if the row exists - caller should handle missing rows.
    /// Supports NULL values to clear the label/model_group.
    pub async fn update_downstream_label(
        &self,
        user_id: &str,
        downstream_id: &str,
        label: Option<&str>,
        model_group_id: Option<&str>,
    ) -> Result<(), PortalStoreError> {
        let client = self.pool.get().await?;
        client
            .execute(
                "UPDATE portal_user_downstreams \
                 SET label = $3, model_group_id = $4 \
                 WHERE user_id = $1 AND downstream_id = $2",
                &[&user_id, &downstream_id, &label, &model_group_id],
            )
            .await?;
        Ok(())
    }

    /// Safe delete: only removes if non-default AND no usage history.
    /// Returns Ok(true) if deleted, Ok(false) if rejected (default or has usage).
    /// Must run in a transaction to ensure consistency.
    pub async fn remove_downstream_binding_safe(
        &self,
        user_id: &str,
        downstream_id: &str,
    ) -> Result<bool, PortalStoreError> {
        let mut client = self.pool.get().await?;
        let transaction = client.transaction().await?;

        // Step 1: Check if the binding exists and get its properties
        let row = transaction
            .query_opt(
                "SELECT is_default, \
                        COALESCE((SELECT COUNT(*) FROM response_history \
                                  WHERE downstream_key_id = $2), 0) AS usage_count \
                 FROM portal_user_downstreams \
                 WHERE user_id = $1 AND downstream_id = $2",
                &[&user_id, &downstream_id],
            )
            .await?;

        let Some(row) = row else {
            // Binding doesn't exist - treat as successful no-op
            transaction.commit().await?;
            return Ok(false);
        };

        let is_default: bool = row.get(0);
        let usage_count: i64 = row.get(1);

        // Step 2: Reject if default or has usage
        if is_default || usage_count > 0 {
            transaction.commit().await?;
            return Ok(false);
        }

        // Step 3: Delete the binding
        let deleted = transaction
            .execute(
                "DELETE FROM portal_user_downstreams \
                 WHERE user_id = $1 AND downstream_id = $2 \
                   AND NOT is_default \
                   AND NOT EXISTS ( \
                     SELECT 1 FROM response_history \
                     WHERE downstream_key_id = $2 \
                   )",
                &[&user_id, &downstream_id],
            )
            .await?;

        transaction.commit().await?;
        Ok(deleted > 0)
    }

    /// Set a downstream binding as the default key.
    /// Clears all other defaults for this user in a transaction to ensure uniqueness.
    /// Silently succeeds even if downstream_id doesn't exist (no rows updated).
    pub async fn set_default_key(
        &self,
        user_id: &str,
        downstream_id: &str,
    ) -> Result<(), PortalStoreError> {
        let mut client = self.pool.get().await?;
        let transaction = client.transaction().await?;

        // Step 1: Clear all defaults for this user
        transaction
            .execute(
                "UPDATE portal_user_downstreams \
                 SET is_default = FALSE \
                 WHERE user_id = $1",
                &[&user_id],
            )
            .await?;

        // Step 2: Set the new default
        transaction
            .execute(
                "UPDATE portal_user_downstreams \
                 SET is_default = TRUE \
                 WHERE user_id = $1 AND downstream_id = $2",
                &[&user_id, &downstream_id],
            )
            .await?;

        transaction.commit().await?;
        Ok(())
    }

    /// Add a binding; setting `is_default` demotes every other row.  Returns
    /// `NotFound` when the user does not exist.
    pub async fn add_downstream_binding(
        &self,
        user_id: &str,
        downstream_id: &str,
        is_default: bool,
    ) -> Result<(), PortalStoreError> {
        let mut client = self.pool.get().await?;
        let transaction = client.transaction().await?;
        let known = transaction
            .query_opt(
                "SELECT 1 FROM portal_users WHERE id = $1",
                &[&user_id],
            )
            .await?
            .is_some();
        if !known {
            return Err(PortalStoreError::NotFound);
        }
        if is_default {
            transaction
                .execute(
                    "UPDATE portal_user_downstreams SET is_default = FALSE WHERE user_id = $1",
                    &[&user_id],
                )
                .await?;
        }
        transaction
            .execute(
                "INSERT INTO portal_user_downstreams (user_id, downstream_id, is_default) \
                 VALUES ($1, $2, $3) \
                 ON CONFLICT (user_id, downstream_id) \
                 DO UPDATE SET is_default = EXCLUDED.is_default",
                &[&user_id, &downstream_id, &is_default],
            )
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    /// Remove a binding; when it was the default and bindings remain, another
    /// row is promoted so the user still has exactly one default.
    pub async fn remove_downstream_binding(
        &self,
        user_id: &str,
        downstream_id: &str,
    ) -> Result<(), PortalStoreError> {
        let mut client = self.pool.get().await?;
        let transaction = client.transaction().await?;
        transaction
            .execute(
                "DELETE FROM portal_user_downstreams WHERE user_id = $1 AND downstream_id = $2",
                &[&user_id, &downstream_id],
            )
            .await?;
        transaction
            .execute(
                "UPDATE portal_user_downstreams SET is_default = TRUE \
                 WHERE user_id = $1 \
                   AND NOT EXISTS (SELECT 1 FROM portal_user_downstreams p2 \
                                   WHERE p2.user_id = $1 AND p2.is_default) \
                   AND downstream_id = (SELECT p3.downstream_id FROM portal_user_downstreams p3 \
                                        WHERE p3.user_id = $1 ORDER BY p3.downstream_id LIMIT 1)",
                &[&user_id],
            )
            .await?;
        transaction.commit().await?;
        Ok(())
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
