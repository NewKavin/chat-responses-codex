-- Executed by initialize_schema before serving requests. Classification is in
-- model_access_store::migrate_access_policies and shares its transaction.
CREATE TABLE IF NOT EXISTS downstream_access_policies (
    downstream_id TEXT PRIMARY KEY REFERENCES downstreams(id) ON DELETE CASCADE,
    subject_kind TEXT NOT NULL CHECK (subject_kind IN ('direct', 'portal')),
    owner_user_id TEXT REFERENCES portal_users(id) ON DELETE SET NULL,
    mode TEXT NOT NULL CHECK (mode IN ('inherit', 'group', 'deny')),
    model_group_id TEXT NOT NULL DEFAULT 'deny-all'
        REFERENCES model_groups(id) ON DELETE SET DEFAULT,
    revision BIGINT NOT NULL DEFAULT 1 CHECK (revision > 0),
    CHECK (subject_kind = 'portal' OR (owner_user_id IS NULL AND mode <> 'inherit')),
    CHECK (mode = 'group' OR model_group_id = 'deny-all')
);
CREATE INDEX IF NOT EXISTS downstream_access_owner_idx
    ON downstream_access_policies(owner_user_id);
CREATE INDEX IF NOT EXISTS downstream_access_group_idx
    ON downstream_access_policies(model_group_id);

CREATE TABLE IF NOT EXISTS downstream_access_migrations (
    downstream_id TEXT NOT NULL,
    migration_version INTEGER NOT NULL,
    classification TEXT NOT NULL,
    before_policy JSONB NOT NULL,
    after_policy JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    resolved_at TIMESTAMPTZ,
    PRIMARY KEY (downstream_id, migration_version)
);
