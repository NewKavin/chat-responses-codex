-- migrations/2026-09-06-fix-model-group-seeds.sql
-- 1) 建 sentinel 组 deny-all：组被删除 / 新建下游未指定权限档时的安全兜底。
--    不能用空数组：model_list_allows(usage.rs:275) 把空列表当"放行全部"，
--    所以"拒绝全部"必须用哨兵值 ["__none__"]。
-- 2) basic / premium 从占位模型改为本部署真实模型（与生产 active upstream 一致）。
-- 幂等：可重复执行，结果不变。

BEGIN;

-- 1. sentinel 组（拒绝全部）
INSERT INTO model_groups (id, name, description, allowed_models)
VALUES ('deny-all', 'Deny All', 'Sentinel group that denies every model (safety fallback)',
        '["__none__"]'::jsonb)
ON CONFLICT (id) DO NOTHING;

-- 2. basic：性价比档，全部为 active upstream 真实模型
UPDATE model_groups
SET name = 'Basic Models',
    description = 'Cost-effective models for development and testing',
    allowed_models = '["deepseek-v4-flash", "deepseek-v4-flash-0731", "deepseek-v4-flash-free", "glm-5.3-flash", "kimi-k3"]'::jsonb
WHERE id = 'basic';

-- 3. premium：旗舰档，全部为 active upstream 真实模型
UPDATE model_groups
SET name = 'Premium Models',
    description = 'Advanced models for production workloads',
    allowed_models = '["glm-5.2", "glm-5.3", "deepseek-v4-pro", "deepseek-v4-pro-0813", "gpt-5.5", "gpt-5.6-luna", "gpt-5.6-sol", "gpt-5.6-terra", "grok-4.5", "grok-4.6", "claude-fable-5", "claude-opus-5", "claude-opus-4-8", "claude-sonnet-5", "qwen3.8-max"]'::jsonb
WHERE id = 'premium';

COMMIT;
