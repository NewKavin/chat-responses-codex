-- 费用限额从「按 Key」改为「按账号」：费用以「费用归属（cost scope）」为单位汇总。
-- scope_id 既可能是门户用户 id，也可能是没有归属用户的直连下游 id。
-- Task 1 先建表；Task 5 在同一文件里追加 Key 级上限折算与 DROP COLUMN（顺序不能颠倒）。
CREATE TABLE IF NOT EXISTS cost_scope_limits (
    scope_id          TEXT PRIMARY KEY,
    daily_limit_cents BIGINT NOT NULL CHECK (daily_limit_cents > 0)
);

-- Task 5：把 Key 级上限折算成账号级上限：每个账号取名下 Key 原上限的最大值。
-- 有归属用户的记到用户名下（COALESCE(p.owner_user_id, d.id)），没有归属的记到 Key 自己名下。
INSERT INTO cost_scope_limits (scope_id, daily_limit_cents)
SELECT DISTINCT COALESCE(p.owner_user_id, d.id), MAX(d.daily_cost_limit_cents)
FROM downstreams d
LEFT JOIN downstream_access_policies p ON p.downstream_id = d.id
WHERE d.daily_cost_limit_cents IS NOT NULL
  AND d.daily_cost_limit_cents > 0
GROUP BY COALESCE(p.owner_user_id, d.id)
ON CONFLICT (scope_id) DO UPDATE
    SET daily_limit_cents = GREATEST(
        cost_scope_limits.daily_limit_cents,
        EXCLUDED.daily_limit_cents
    );

-- 折算完成后删列。删掉之后不可能再有任何 Key 级残留限制。
ALTER TABLE downstreams DROP COLUMN IF EXISTS daily_cost_limit_cents;
