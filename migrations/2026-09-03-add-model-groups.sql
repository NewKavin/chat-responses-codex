-- migrations/2026-09-03-add-model-groups.sql
-- 添加模型分组功能

BEGIN;

-- 创建模型分组表
CREATE TABLE IF NOT EXISTS model_groups (
  id TEXT PRIMARY KEY CHECK (id ~ '^[a-z0-9-]+$'),
  name TEXT NOT NULL,
  description TEXT,
  allowed_models JSONB NOT NULL,
  created_at TIMESTAMPTZ DEFAULT NOW(),
  updated_at TIMESTAMPTZ DEFAULT NOW()
);

-- 插入初始数据
INSERT INTO model_groups (id, name, description, allowed_models) VALUES
  ('basic', 'Basic Models', 'Cost-effective models for development and testing',
   '["gpt-3.5-turbo", "claude-3-haiku"]'::jsonb),
  ('premium', 'Premium Models', 'Advanced models for production workloads',
   '["gpt-4", "gpt-4-turbo", "claude-3-opus", "claude-3.5-sonnet", "claude-3-sonnet"]'::jsonb),
  ('all', 'All Models', 'Unrestricted access to all available models',
   '["*"]'::jsonb)
ON CONFLICT (id) DO NOTHING;

-- 添加 model_group_id 列到 portal_user_downstreams
ALTER TABLE portal_user_downstreams
ADD COLUMN IF NOT EXISTS model_group_id TEXT DEFAULT 'basic'
REFERENCES model_groups(id) ON DELETE SET DEFAULT;

-- 添加索引
CREATE INDEX IF NOT EXISTS idx_portal_user_downstreams_model_group
ON portal_user_downstreams(model_group_id);

-- 更新现有的 key- 前缀 key 为 'all' 分组（向后兼容）
-- 注意：这需要连接 downstreams 表，假设该表存在
DO $$
BEGIN
  IF EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'downstreams') THEN
    UPDATE portal_user_downstreams pud
    SET model_group_id = 'all'
    WHERE EXISTS (
      SELECT 1 FROM downstreams d
      WHERE d.id = pud.downstream_id
        AND d.plaintext_key LIKE 'key-%'
    );
  END IF;
END $$;

COMMIT;
