# 运维升级说明：模型分组接管权限（retire model_allowlist）

适用版本：本 tar 包（含 W1/W2、M1/M2/M3、T10–T15；**不含** T16 删表删列）。
升级形态：**内网一次性 tar 包**，启动时自动完成种子修正、数据迁移、权限兜底，无需手工跑 SQL。

## 0. 升级前后职责

- 升级前：**只做备份 + Codex 目录快照**（唯一人工前置）。
- 升级后：核对启动日志、数据、目录 diff；观察期结束后下个版本再做 T16 删表删列。

## 1. 升级前（在旧版本上执行）

```bash
# 1) 备份三张相关表（可重复执行；备份文件不留库内）
docker exec chat-responses-codex-postgres pg_dump -U chat_responses_codex \
  -d chat_responses_codex \
  -t downstreams -t downstream_model_allowlist -t model_groups \
  > /backup/pre-model-group-$(date +%F).sql

# 2) Codex 目录快照（每把在用 key 存一份 /v1/models?format=codex 的 slug 列表）
scripts/catalog-diff.sh snapshot before

# 3) 可选的迁移前基线
docker exec chat-responses-codex-postgres psql -U chat_responses_codex -d chat_responses_codex \
  -c "SELECT COUNT(*) FROM downstreams WHERE model_group_id IS NULL;"
```

## 2. 载入 tar 包并重启

正常启动即可。启动日志必须出现以下三类中的对应行（三种都有实现，按库状态只出现其一或其二）：

```
INFO gateway_core::state::postgres: model allowlist migration: 12 downstreams -> all group, 5 auto groups created, 23 bound
INFO gateway_core::state::postgres: model allowlist migration: nothing to migrate (all downstreams already grouped)
WARN gateway_core::state::postgres: model allowlist migration: table downstream_model_allowlist absent, skipping
```

无变化时也会打一行：区分“跑了且无事可做”和“根本没跑”。

## 3. 升级后核对

```bash
# 1) Codex 目录：逐 key 对比，期望零差异
scripts/catalog-diff.sh snapshot after
scripts/catalog-diff.sh diff

# 2) §2.3 校验查询：期望 0 行（迁移前后有效模型集合逐位一致）
docker exec chat-responses-codex-postgres psql -U chat_responses_codex -d chat_responses_codex <<'SQL'
CREATE TEMP TABLE pre_migration_snapshot AS
SELECT d.id AS downstream_id,
       COALESCE(
         (SELECT ARRAY_AGG(DISTINCT LOWER(TRIM(a.model_slug)) ORDER BY LOWER(TRIM(a.model_slug)))
          FROM downstream_model_allowlist a
          WHERE a.downstream_id = d.id AND TRIM(a.model_slug) <> ''),
         ARRAY['*']
       ) AS effective_models
FROM downstreams d
WHERE d.model_group_id IS NULL;

SELECT s.downstream_id, s.effective_models AS before,
       ARRAY(SELECT LOWER(JSONB_ARRAY_ELEMENTS_TEXT(mg.allowed_models)) ORDER BY 1) AS after
FROM pre_migration_snapshot s
JOIN downstreams d ON d.id = s.downstream_id
JOIN model_groups mg ON mg.id = d.model_group_id
WHERE s.effective_models IS DISTINCT FROM
      ARRAY(SELECT LOWER(JSONB_ARRAY_ELEMENTS_TEXT(mg.allowed_models)) ORDER BY 1);
SQL

# 3) 未绑组必须为 0；列必须 NOT NULL + DEFAULT 'deny-all'；外键必须 ON DELETE SET DEFAULT
docker exec chat-responses-codex-postgres psql -U chat_responses_codex -d chat_responses_codex \
  -c "SELECT COUNT(*) FROM downstreams WHERE model_group_id IS NULL;"
docker exec chat-responses-codex-postgres psql -U chat_responses_codex -d chat_responses_codex \
  -c "SELECT is_nullable, column_default FROM information_schema.columns
      WHERE table_name='downstreams' AND column_name='model_group_id';"
docker exec chat-responses-codex-postgres psql -U chat_responses_codex -d chat_responses_codex \
  -c "SELECT confdeltype FROM pg_constraint WHERE conname='fk_downstream_model_group';"
# confdeltype = 'd' 才是 SET DEFAULT；'n' = SET NULL（异常）

# 4) 抽查 3 个 key：迁移前能用的模型仍能用，不能用的仍不能用（含一个 Codex key 实跑一次会话）

# 5) 新建下游不指定 model_group_id → 落 deny-all，用该 key 请求任何模型必须 403
```

## 4. 回滚（T16 之前有效；表仍在，才有这条退路）

代码回滚 = 换回旧 tar 包。数据不回滚；旧代码仍按“组优先、解析失败回退白名单”工作。
真要退回白名单语义：

```sql
UPDATE downstreams SET model_group_id = NULL
WHERE model_group_id LIKE 'auto-%' OR model_group_id = 'all';
```

只清本次迁移写入的组绑定，手工绑定的 basic / premium 不动；执行完重启即恢复白名单语义。
注意：本次同时给 `model_group_id` 加了 NOT NULL + DEFAULT 'deny-all' 并改了外键
`ON DELETE SET DEFAULT`。回滚 SQL 会先被 NOT NULL 与外键挡住，需先撤约束：

```sql
-- 回滚前先解除 T15 约束（旧代码不认识 SET DEFAULT 兜底）
ALTER TABLE downstreams DROP CONSTRAINT IF EXISTS fk_downstream_model_group;
ALTER TABLE downstreams ALTER COLUMN model_group_id DROP NOT NULL;
ALTER TABLE downstreams ALTER COLUMN model_group_id DROP DEFAULT;
ALTER TABLE downstreams ADD CONSTRAINT fk_downstream_model_group
  FOREIGN KEY (model_group_id) REFERENCES model_groups(id) ON DELETE SET NULL;
UPDATE downstreams SET model_group_id = NULL
WHERE model_group_id LIKE 'auto-%' OR model_group_id = 'all';
```

再次升级时新代码会把约束与绑定重新建好（幂等）。

## 5. 观察期注意事项

- `downstream_model_allowlist` 表保留、`postgres.rs` 双写保持（回滚数据路径），**不要手工删**。
- 不要手工改迁移生成的 `auto-*` 组的 `allowed_models`（语义会与回滚快照脱节）。
- 下个版本（T16）将删除 `model_allowlist` 字段与 `downstream_model_allowlist` 表。
