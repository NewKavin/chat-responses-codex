-- migrations/2026-09-03-migrate-existing-keys-to-default.sql
-- 确保现有用户的 key 有默认标记
--
-- 运行时机：在 Task 1 的 migration (2026-09-03-add-key-labels.sql) 之后
-- 影响：无停机，幂等操作
-- 回滚：如需回滚，运行: UPDATE portal_user_downstreams SET is_default = false;

BEGIN;

-- 步骤 1: 对于只有一个 key 的用户，将其设为默认
UPDATE portal_user_downstreams
SET is_default = true
WHERE user_id IN (
    SELECT user_id
    FROM portal_user_downstreams
    GROUP BY user_id
    HAVING COUNT(*) = 1
)
AND is_default = false;

-- 步骤 2: 对于有多个 key 但没有默认的用户，将最早创建的 key 设为默认
-- 使用 downstream_id 作为 tie-breaker（如果 created_at 相同或为 NULL）
WITH users_without_default AS (
    SELECT user_id
    FROM portal_user_downstreams
    GROUP BY user_id
    HAVING COUNT(*) > 1
       AND SUM(CASE WHEN is_default THEN 1 ELSE 0 END) = 0
),
first_keys AS (
    SELECT DISTINCT ON (d.user_id)
        d.user_id,
        d.downstream_id
    FROM portal_user_downstreams d
    INNER JOIN users_without_default u ON d.user_id = u.user_id
    ORDER BY d.user_id,
             COALESCE(d.created_at, NOW()),
             d.downstream_id
)
UPDATE portal_user_downstreams
SET is_default = true
FROM first_keys
WHERE portal_user_downstreams.user_id = first_keys.user_id
  AND portal_user_downstreams.downstream_id = first_keys.downstream_id;

-- 验证：确保每个用户恰好有一个默认 key（如果该用户有 key 的话）
DO $$
DECLARE
    bad_count INTEGER;
BEGIN
    SELECT COUNT(*) INTO bad_count
    FROM (
        SELECT user_id
        FROM portal_user_downstreams
        GROUP BY user_id
        HAVING SUM(CASE WHEN is_default THEN 1 ELSE 0 END) != 1
    ) subq;

    IF bad_count > 0 THEN
        RAISE EXCEPTION 'Migration failed: % users do not have exactly one default key', bad_count;
    END IF;
END $$;

COMMIT;
