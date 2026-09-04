-- migrations/2026-09-03-add-key-labels.sql
-- 此迁移无需停机，可在运行时执行

BEGIN;

-- 添加 label 列（允许 NULL，兼容现有数据）
ALTER TABLE portal_user_downstreams
ADD COLUMN IF NOT EXISTS label TEXT;

-- 添加约束：最大 100 字符
ALTER TABLE portal_user_downstreams
ADD CONSTRAINT IF NOT EXISTS label_max_length
CHECK (label IS NULL OR char_length(label) <= 100);

-- 添加模型分组列（为模型分组功能预留，默认 'basic'）
ALTER TABLE portal_user_downstreams
ADD COLUMN IF NOT EXISTS model_group_id TEXT DEFAULT 'basic';

-- 添加索引（优化查询）
CREATE INDEX IF NOT EXISTS idx_portal_user_downstreams_user_id
ON portal_user_downstreams(user_id);

-- 添加创建时间列
ALTER TABLE portal_user_downstreams
ADD COLUMN IF NOT EXISTS created_at TIMESTAMPTZ DEFAULT NOW();

-- 添加 response_history 索引（优化使用统计查询）
CREATE INDEX IF NOT EXISTS idx_response_history_downstream_created
ON response_history(downstream_key_id, created_at DESC);

COMMIT;
