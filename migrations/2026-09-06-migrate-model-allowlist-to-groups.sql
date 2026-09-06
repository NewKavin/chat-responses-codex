-- migrations/2026-09-06-migrate-model-allowlist-to-groups.sql
-- 阶段 2：把历史 downstream.model_allowlist 迁移为模型分组。
-- 幂等：可重复执行；只处理 model_group_id IS NULL 的下游。
-- 分组策略：空白名单 → all；非空 → auto-<sha256(小写排序模型拼接)[..8]>（相同集合共用一组）。

BEGIN;

-- 临时表清理：保证同连接内可重复执行（幂等）。
DROP TABLE IF EXISTS _sets;
DROP TABLE IF EXISTS _keyed;

-- 1. 空白名单 → all
UPDATE downstreams d
SET model_group_id = 'all'
WHERE d.model_group_id IS NULL
  AND NOT EXISTS (
    SELECT 1 FROM downstream_model_allowlist a
    WHERE a.downstream_id = d.id AND TRIM(a.model_slug) <> ''
  );

-- 2. 非空白名单：按内容建组（保留原拼写，按小写去重，按小写排序保证哈希稳定）
CREATE TEMP TABLE _sets AS
SELECT
  a.downstream_id,
  ARRAY(
    SELECT DISTINCT ON (LOWER(TRIM(m.model_slug))) TRIM(m.model_slug)
    FROM downstream_model_allowlist m
    WHERE m.downstream_id = a.downstream_id AND TRIM(m.model_slug) <> ''
    ORDER BY LOWER(TRIM(m.model_slug))
  ) AS models
FROM downstream_model_allowlist a
GROUP BY a.downstream_id;

CREATE TEMP TABLE _keyed AS
SELECT
  downstream_id,
  models,
  'auto-' || SUBSTRING(
    ENCODE(SHA256(CONVERT_TO(LOWER(ARRAY_TO_STRING(models, ',')), 'UTF8')), 'hex'), 1, 8
  ) AS gid
FROM _sets
WHERE CARDINALITY(models) > 0;

INSERT INTO model_groups (id, name, description, allowed_models)
SELECT DISTINCT
  k.gid,
  '迁移分组 ' || k.gid,
  '由 model_allowlist 自动迁移（' || CARDINALITY(k.models) || ' 个模型）',
  TO_JSONB(k.models)
FROM _keyed k
ON CONFLICT (id) DO NOTHING;

UPDATE downstreams d
SET model_group_id = k.gid
FROM _keyed k
WHERE d.id = k.downstream_id AND d.model_group_id IS NULL;

COMMIT;
