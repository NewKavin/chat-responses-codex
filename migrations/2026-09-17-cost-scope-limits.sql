-- 费用限额从「按 Key」改为「按账号」：费用以「费用归属（cost scope）」为单位汇总。
-- scope_id 既可能是门户用户 id，也可能是没有归属用户的直连下游 id。
-- Task 1 先建表；Task 5 在同一文件里追加 Key 级上限折算与 DROP COLUMN（顺序不能颠倒）。
CREATE TABLE IF NOT EXISTS cost_scope_limits (
    scope_id          TEXT PRIMARY KEY,
    daily_limit_cents BIGINT NOT NULL CHECK (daily_limit_cents > 0)
);
