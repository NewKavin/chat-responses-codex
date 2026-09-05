-- migrations/2026-09-05-add-downstream-model-group-id.sql
-- 为 downstreams 表添加 model_group_id 字段，支持通过模型分组统一管理下游的模型权限

BEGIN;

-- 添加 model_group_id 列到 downstreams 表
-- 允许为 NULL（向后兼容），优先级：model_group_id > model_allowlist
ALTER TABLE downstreams
ADD COLUMN IF NOT EXISTS model_group_id TEXT;

-- 添加外键约束（可选，模型分组删除时下游回退到 model_allowlist）
ALTER TABLE downstreams
ADD CONSTRAINT fk_downstream_model_group
FOREIGN KEY (model_group_id)
REFERENCES model_groups(id)
ON DELETE SET NULL;

-- 添加索引加速查询
CREATE INDEX IF NOT EXISTS idx_downstreams_model_group_id
ON downstreams(model_group_id);

-- 添加注释
COMMENT ON COLUMN downstreams.model_group_id IS
'Model group ID. If set, the downstream inherits allowed_models from the group. Priority: model_group_id > model_allowlist (for backward compatibility).';

COMMIT;
