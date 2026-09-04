BEGIN;

-- 用户对模型分组的访问权限表
CREATE TABLE IF NOT EXISTS portal_user_model_groups (
  user_id TEXT NOT NULL REFERENCES portal_users(id) ON DELETE CASCADE,
  model_group_id TEXT NOT NULL REFERENCES model_groups(id) ON DELETE CASCADE,
  granted_at TIMESTAMPTZ DEFAULT NOW(),
  granted_by TEXT,  -- 授权者 user_id，可选
  PRIMARY KEY (user_id, model_group_id)
);

CREATE INDEX IF NOT EXISTS idx_portal_user_model_groups_user
ON portal_user_model_groups(user_id);

CREATE INDEX IF NOT EXISTS idx_portal_user_model_groups_group
ON portal_user_model_groups(model_group_id);

-- 向后兼容：给已使用 premium/all 的用户追加授权
INSERT INTO portal_user_model_groups (user_id, model_group_id)
SELECT DISTINCT pud.user_id, pud.model_group_id
FROM portal_user_downstreams pud
WHERE pud.model_group_id IS NOT NULL
  AND pud.model_group_id != 'basic'
ON CONFLICT (user_id, model_group_id) DO NOTHING;

COMMIT;
