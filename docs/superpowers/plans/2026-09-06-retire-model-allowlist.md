# 下线 `model_allowlist`，统一由模型分组接管 — 方案

状态：待开发（2026-09-06 定稿，三项决策已拍板）
前置：外部草案 `docs/investigations/model-allowlist-deprecation-plan.md` 已作废并删除，其 4 处错误的修正见附录 A

## 已拍板的三项决策

1. **默认权限档 = sentinel 组 `deny-all`**（`allowed_models = ["__none__"]`）。新建下游、组被删除时都落到它。不用 `basic`，绝不用 `all`。
2. **`basic` / `premium` 的占位种子模型改成本部署真实模型**（阶段 0 执行，见 §3.0）。
3. **`apply_model_qualification` 改为写入下游所绑分组的 `allowed_models`**，不再写 `model_allowlist`（阶段 3 T11，含共享组防外溢规则，见 §3.3）。

---

## 0. 结论先说

`model_allowlist` **现在还有用，而且是好几个接口的唯一数据源**，直接删会出线上事故。但它确实该退休：分组已经接管了主请求校验，剩下的读路径不看分组，属于**现存 bug**（已绑组的 key 在门户/Codex 目录/统计里看到的是旧白名单）。

对"给老 key 挂一个默认分组保持可用"这件事，**不能挂 `basic`**。`basic` 里存的是 `gpt-3.5-turbo` / `claude-3-haiku`（`src/state/postgres.rs:2113` 与 `migrations/2026-09-03-add-model-groups.sql` 的占位种子数据），本部署一个都没有。挂上去等于把老 key 全封死。老 key 必须挂**内容等价的自动分组**（空白名单 → `all`）；`basic` 的占位模型另行修正（阶段 0）。

正确顺序：补齐读路径 → 迁移数据 → 停写 → 删列。四个阶段，每阶段独立可发布、可回滚。

---

## 1. 现状（代码证据）

### 1.1 已经看分组的路径（3 处）

| 位置 | 行为 |
|---|---|
| `src/server/gateway.rs:5570` | 主请求校验。组优先，解析失败 fail-closed（500 `model_group_check_failed`） |
| `src/server/gateway.rs:3282` | Anthropic `count_tokens`。同上 |
| `src/state.rs:6220` | `available_models_for_downstream`（`/v1/models` 普通格式、troubleshooting）。组解析失败返回空列表 |

三处都是同一段 40 行逻辑复制粘贴，无 portal store（文件模式）时回退白名单。

### 1.2 只看 `model_allowlist`、完全不看分组的路径（9 处，全是 bug）

| 位置 | 函数 | 症状 |
|---|---|---|
| `src/server/gateway.rs:3056` | `list_models_codex_format` | **Codex 模型目录错误**，详见 §2 |
| `src/server/portal.rs:255` | `portal_quota` | 门户"可用模型"显示错误（Overview / QuotaDetails / Playground / Integration 全部消费这个字段） |
| `src/server/portal.rs:498` | `portal_model_probe` | 探测范围错误 |
| `src/state/usage.rs:573` | `compute_model_stats` | 模型统计漏算/多算 |
| `src/state/usage.rs:635-650` | `compute_portal_model_context_limits` | 上下文窗口清单错误 |
| `src/state/log_queries.rs:219,238` | `build_downstream_usage_summary`（文件模式） | `total_models` / `active_models` 算错 |
| `src/state/postgres.rs:637,674` | `downstream_usage_summary`（DB 模式，**草案漏掉**） | 同上，直接 SQL 查 `downstream_model_allowlist` 表 |
| `src/state.rs:6145` | `reconcile_dialect_profiles` | 能力探测的"是否对下游暴露"判断错误 |
| `src/state.rs:6298` | `downstream_visible_models`（`/admin/models?scope=visible`） | 后台可见模型列表错误 |
| `src/state.rs:6733` | `queue_capability_probes_for_downstream_model` | 探测排队判断错误 |
| `crates/gateway-core/src/admin.rs:247` | `DownstreamFormView::from_downstream` | 旧版 SSR 表单回显错误（crate 边界拿不到 portal store） |

DB 模式实际走 `postgres.rs:616 downstream_usage_summary`，文件模式走 `log_queries.rs:166`，两套逻辑都要改。

### 1.3 写 `model_allowlist` 的路径（5 处，注意第 4 处）

| 位置 | 性质 |
|---|---|
| `src/server/admin.rs:2302` | `apply_downstream_updates`，管理端 PUT |
| `src/server/admin.rs:2709` | `BATCH_UPDATE_DOWNSTREAM_ALLOWED_FIELDS` 批量改 |
| `src/state.rs:8401` | `update_downstream_by_id` |
| `src/state.rs:7230` | **`apply_model_qualification`（草案漏掉）**：模型资格审定把"保留下来的模型"写进目标下游的 `model_allowlist`。这是**功能写入**，不是管理员手改，删字段前必须改成写分组或明确废弃 |
| `src/state/postgres.rs:1417-1433` | 持久化：全删重插 `downstream_model_allowlist` 表 |

### 1.4 两层分组，都在生效（草案说"绑定组不强制"，错了）

- **key 级**：`downstreams.model_group_id` → `gateway.rs:5570`
- **绑定级**：`portal_user_downstreams.model_group_id` → `gateway.rs:5664` 调 `portal_store.rs:1153 get_key_allowed_models`

绑定级语义（`portal_store.rs:1153-1197`）：有绑定且组可解析 → 用该组；有绑定但组为 NULL → **fail-closed 落到 `basic`**；无绑定（管理端直接建的 key）→ 返回空列表 = 不限制。

所以门户创建的 key 要过**两道**分组闸门。本方案只动 key 级，但 §3.0 必须先修 `basic` 的种子数据，否则绑定级那道闸门已经在用占位模型封人。

另外 `get_key_allowed_models` 的 SQL 是 `WHERE d.downstream_id = $1` + `query_opt`，**没有按 user 收敛**：同一个 downstream 被多个用户绑定时命中哪一行不确定。本方案不改，单独记一笔。

### 1.5 语义对照与匹配规则

| `model_allowlist` | 等价分组 |
|---|---|
| 空数组 | `allowed_models = ["*"]` |
| `["gpt-4","claude-3-opus"]` | `allowed_models` 同值 |

- `model_list_allows`（`src/state/usage.rs:275`）：空列表 = 放行全部；含 `"*"` = 放行全部；否则走 `portal_model_is_allowed` 归一化匹配。
- `portal_model_is_allowed`（`usage.rs:247`）内部用 `normalize_model_name`，**无条件 `to_ascii_lowercase()`**。所以白名单匹配**永远是大小写不敏感的**，与 `model_case_insensitive_matching`（默认 `true`，`src/state/types.rs:140`）无关。草案说"迁移前必须确认该开关否则 LOWER() 会改变行为"——不成立。
- 但 `ModelGroup::allows_model`（`portal_store.rs:83`）是 `Vec::contains` **精确匹配、大小写敏感**，目前只在测试里用。谁把它接到请求路径上就会引入不一致，开发时不要碰它。
- `model_groups.id` 有 `CHECK (id ~ '^[a-z0-9-]+$')`：自动生成的组 id 必须小写；`allowed_models` 的**值**没有这个约束，要保留原始拼写。
- **空列表 = 放行全部**这条语义有个坑：将来想表达"什么都不许"，不能用空数组，见 §3.4。

---

## 2. Codex 模型目录（`~/.codex/model-catalog.json`）的影响

这是本次最该优先修的一处。

### 2.1 链路

`~/.codex/config.toml` 里 `model_catalog_json = "model-catalog.json"`、`base_url = http://<gateway>/v1`、`model = "deepseek-v4-flash-0731"`。目录文件按 `docs/codex-integration-guide.md:507` 的流程，用下游 key 从 **`/v1/models?format=codex`** 拉取后整份落盘。该接口就是 `list_models_codex_format`（`gateway.rs:3045`），它读的是 `downstream.model_allowlist`（`:3056`），**不看分组**。

后果：已绑组的 key 生成的目录是旧白名单。目录里多出来的模型 → Codex 能选、发请求被主路径 403；目录里少掉的模型 → Codex 根本不给选，且 `config.toml` 的 `model` 如果不在目录里，Codex **启动阶段就报错**，请求到不了网关。子代理更敏感：`~/.codex/agents/default.toml` 独立加载同一份目录（`DEPLOYMENT.md:796`），模型名不一致会在委派阶段被拒。

### 2.2 slug 拼写：迁移不会改目录，但要说清为什么

`codex_exposed_models`（`gateway.rs:2953`）的暴露规则：

- 白名单为空 → 目录 = 所有 active upstream 当前发布的模型。
- 白名单非空 → 用**小写 key** 与 upstream 模型做匹配（`:3009` `to_ascii_lowercase`）；命中的条目用 **upstream 侧拼写**（`from_mapping` 的映射标签原样、否则 canonical 小写形式）；未命中的白名单条目**按小写 key push**（`:3032`）。

所以：迁移时对 `allowed_models` 做 `LOWER()` 既不会改变匹配结果，也不会改变目录 slug。但仍然**建议只 `TRIM` 不 `LOWER`**，理由是保留原拼写对门户配额页、探测响应、以及 `ModelGroup::allows_model` 这类精确匹配路径更安全，成本为零。

用户本机目录里的 22 个 slug（`GLM-5.2`、`claude-fable-5`、`claude-opus-4-8`、`claude-opus-5`、`claude-sonnet-5`、`deepseek-v4-flash`、`deepseek-v4-flash-0731`、`deepseek-v4-flash-0731-262k-think`、`deepseek-v4-flash-free`、`deepseek-v4-pro`、`deepseek-v4-pro-0813`、`deepseek-v4-pro-free`、`glm-5.3`、`glm-5.3-flash`、`gpt-5.5`、`gpt-5.6-luna`、`gpt-5.6-sol`、`gpt-5.6-terra`、`grok-4.5`、`grok-4.6`、`kimi-k3`、`qwen3.8-max`）里 `GLM-5.2` 是唯一大写的，说明它来自 upstream 映射标签（`from_mapping` 原样暴露）。**分组的 `allowed_models` 若把它写成 `glm-5.2`，匹配照旧成立，目录仍然输出 `GLM-5.2`**（拼写来自 upstream 侧），不影响现有 `config.toml`。

### 2.3 强制验收项

阶段 1、阶段 2 都必须做这条端到端对比，任何一步都不许跳过：

```bash
# 迁移/改代码前后各跑一次，逐 key 对比
curl -s -H "Authorization: Bearer <downstream_key>" \
  "http://<gateway>/v1/models?format=codex" | jq -S '[.models[].slug]|sort' > /tmp/catalog-before.json
# ...变更后...
diff /tmp/catalog-before.json /tmp/catalog-after.json   # 期望：无差异

# 现有 Codex 配置仍然自洽
jq -r '.models[].slug' ~/.codex/model-catalog.json | grep -Fx "$(grep -oP '^model\s*=\s*"\K[^"]+' ~/.codex/config.toml)"
codex --strict-config doctor --summary
```

阶段 1 的目标就是让"已绑组的 key"这条 diff **从有差异变成符合分组预期**；未绑组的 key 必须逐位无差异。

---

## 3. 分阶段方案

### 3.0 阶段 0：修种子数据 + 建 sentinel 组（先做，很小，但拦事故）

这一步既有 SQL 也有代码：种子数据写在**两个地方**，只改一个不够。

- `src/state/postgres.rs:2113`（`SCHEMA_SQL`，启动时执行，带 `ON CONFLICT (id) DO NOTHING`）→ 只影响**新库**
- `migrations/2026-09-03-add-model-groups.sql` → 历史留档，不再重跑

所以既有库必须靠一条新的 `UPDATE` migration 修，新库靠改 `SCHEMA_SQL` 修，两处内容保持一致。

**T0-1 新建 `migrations/2026-09-06-fix-model-group-seeds.sql`**

```sql
BEGIN;

-- 1. sentinel 组：组被删除 / 新建下游未指定时的安全兜底档。
--    注意不能用空数组：model_list_allows(src/state/usage.rs:275) 把空列表当"放行全部"，
--    所以"拒绝全部"必须用一个匹配不到任何真实模型的占位 slug 表达。
INSERT INTO model_groups (id, name, description, allowed_models) VALUES
  ('deny-all', '禁止所有模型', '安全兜底档：新建下游未指定分组、或所绑分组被删除时落到这里', '["__none__"]'::jsonb)
ON CONFLICT (id) DO NOTHING;

-- 2. 把 basic / premium 的占位模型换成本部署真实模型。
--    下面的清单以 2026-09-06 的 live 目录为准，执行前用 GET /admin/models 复核一遍，
--    不在 active upstream 里的 slug 删掉。
UPDATE model_groups
SET allowed_models = '["deepseek-v4-flash","deepseek-v4-flash-0731","deepseek-v4-flash-free","glm-5.3-flash","kimi-k3"]'::jsonb,
    name = '基础模型',
    description = '低成本 / 快响应档，门户新建 key 的默认可选档',
    updated_at = NOW()
WHERE id = 'basic';

UPDATE model_groups
SET allowed_models = '["GLM-5.2","glm-5.3","deepseek-v4-pro","deepseek-v4-pro-0813","gpt-5.5","gpt-5.6-luna","gpt-5.6-sol","gpt-5.6-terra","grok-4.5","grok-4.6","claude-fable-5","claude-opus-5","claude-opus-4-8","claude-sonnet-5","qwen3.8-max"]'::jsonb,
    name = '高级模型',
    description = '高成本 / 强能力档',
    updated_at = NOW()
WHERE id = 'premium';

COMMIT;
```

拼写说明：`GLM-5.2` 保留大写是因为它来自 upstream 映射标签（`from_mapping` 原样暴露）。分组匹配走 `model_list_allows` 永远大小写不敏感，写大写或小写都能命中；保留 live 目录的拼写只是让后台界面和探测响应看起来一致。

**T0-2 同步改 `src/state/postgres.rs:2113` 的 `SCHEMA_SQL` 种子**，让新库初始化出来的 `basic` / `premium` / `deny-all` 与上面的 migration 完全一致（`ON CONFLICT DO NOTHING` 保持不动，别改成 upsert，那会在每次启动时覆盖运维的手工调整）。

**为什么必须先做**：`portal_user_downstreams` 那层闸门（`gateway.rs:5664` → `portal_store.rs:1153`）默认就落 `basic`，而 `basic` 现在与真实模型零交集。也就是说门户新建的 key 现在这一层已经在封人，跟本次下线白名单无关，属于既有故障。

**T0-3 其余前置**

- 备份：`pg_dump -t downstream_model_allowlist -t downstreams -t model_groups`
- 快照 Codex 目录：对每个在用 key 存一份 `/v1/models?format=codex` 的 slug 列表（§2.3）
- 验证：`SELECT id, name, allowed_models FROM model_groups ORDER BY id;` 四个组内容符合预期；门户新建一个 `basic` key，实跑一次会话能通

### 3.1 阶段 1：让分组成为所有读路径的唯一事实源（纯修 bug，独立发布）

**1.1 抽统一解析函数**（消掉 §1.1 的三处复制粘贴）

```rust
// src/state.rs
impl AppState {
    /// 解析下游的有效模型白名单：model_group_id 优先，回退 model_allowlist。
    /// Err = 组已配置但解析失败，调用方按自己的语义决定 fail-closed 还是降级。
    pub async fn effective_model_allowlist(
        &self,
        downstream: &DownstreamConfig,
    ) -> Result<Vec<String>, String> {
        if downstream.model_group_id.is_none() {
            return Ok(downstream.model_allowlist.clone());
        }
        match self.portal_store() {
            Some(store) => downstream.get_allowed_models(store.as_ref()).await,
            // 文件模式：组不可用，回退白名单，不要让每个请求都失败
            None => Ok(downstream.model_allowlist.clone()),
        }
    }
}
```

注意 `DownstreamConfig::get_allowed_models`（`types.rs:1326`）当前的行为是"组查不到就 warn 并回退白名单"，**返回 Ok**。`gateway.rs:5570` 那套 fail-closed 实际只在 store 层报错时触发。这个语义分歧要在本阶段一并理清：
- `get_allowed_models` 保持"组不存在 → 回退白名单"（阶段 4 删白名单时这条分支才消失）；
- `effective_model_allowlist` 原样透传 Err，由调用方处理。

**1.2 改造判据**

- **写路径 / 权限判定**：`Err` 必须 fail-closed（拒绝请求），沿用现有错误码 `model_group_check_failed`。
- **纯展示 / 统计**：`Err` 降级为"当作空列表"并 `warn`，不要因为组查不到就 500 掉一个报表接口。

| 位置 | 改法 | `Err` 处理 |
|---|---|---|
| `gateway.rs:3056` | 解析后传给 `codex_exposed_models` | **fail-closed**（权限相关目录） |
| `gateway.rs:5570` / `:3282` / `state.rs:6220` | 改调用新函数，各自保留现有错误响应格式（一个 `GatewayError`+usage log，一个 `into_anthropic_response`，一个返回空 Vec） | 不变 |
| `portal.rs:255` | 替换响应里 `model_allowlist` 字段的**值**，字段名不动 | 降级 + warn |
| `portal.rs:498` | 传给 `build_model_probe_response` | 降级 |
| `usage.rs:573` / `:635` | 有 `&self`，直接调 | 降级 |
| `log_queries.rs:219,238` | 纯函数拿不到 store：签名加 `effective_allowlist: &[String]`，调用侧解析后传入 | 调用方决定 |
| `postgres.rs:637,674` | SQL 直查 `downstream_model_allowlist`：改成由调用侧传入已解析的 allowlist 做参数化过滤（`= ANY($n)`），或整体改走 `log_queries` 同一套逻辑 | 调用方决定 |
| `state.rs:6145` / `:6298` / `:6733` | 循环体内逐 downstream 解析；**注意 N+1**：先按 `model_group_id` 收集去重后批量查一次，再在循环里查表 | 降级（跳过该 downstream 并 warn） |
| `gateway-core/src/admin.rs:247` | crate 边界拿不到 store。加 TODO，保留原样，阶段 4 一起处理 | — |

**1.3 缓存**

阶段 1 把组查询从 3 处扩到 12 处，`/models`、配额、统计都是高频。先不加缓存，压测看 P99；成为瓶颈再在 `PortalStore` 加 10–30s TTL 组缓存，失效点挂在 `create/update/delete_model_group`。`state.rs:6145/6298` 三处循环必须先做批量查询，否则下游数 × 上游模型数会打爆 DB。

**1.4 验收**

- 已绑组的下游：Codex 目录（`/v1/models?format=codex`）、门户配额、模型统计、上下文清单、`/admin/models?scope=visible` 五处与组内容一致。
- 未绑组的下游：全部接口逐位不变（含 §2.3 的 diff 无差异）。
- 组被删除（`ON DELETE SET NULL` → `model_group_id = NULL`）：回退白名单，不 500。
- 文件模式（无 portal store）：行为不变。
- 既有测试全绿：`tests/gateway/model_permission_validation.rs`、`tests/downstream_model_groups.rs`、`tests/portal_api.rs`、`tests/downstream_quota.rs`、`tests/troubleshooting.rs`、`tests/capability_probe.rs`。

### 3.2 阶段 2：迁移历史 key 到分组

**目标：行为逐位不变。**

**2.1 分组策略**：按白名单内容去重，相同集合共用一个组（组数 = 白名单种类数，通常远小于 key 数，还能顺带暴露"这些 key 其实是同一档"）。

- 空白名单 → 挂现有 `all`（`["*"]`）。
- 非空 → `auto-<sha256(排序后小写模型名拼接)[..8]>`，满足 id 的小写约束；`allowed_models` **存原始拼写**（只 TRIM、按小写去重）。
- 不要每个 key 建一个专属组（组爆炸）；**绝对不要**全挂 `all`（权限放大）。

**2.2 迁移 SQL**（`migrations/2026-09-06-migrate-model-allowlist-to-groups.sql`）

```sql
BEGIN;

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
```

注意 `SHA256()` 需要 pg13+ 且参数是 `bytea`，用 `CONVERT_TO(..., 'UTF8')` 而不是 `::bytea` 强转（后者对非 ASCII 会报错）。哈希只对小写形式取，保证 `["GLM-5.2"]` 与 `["glm-5.2"]` 合并成同一组。

**2.3 迁移前后校验**（必须留证据）

```sql
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

-- 迁移后：期望 0 行
SELECT s.downstream_id, s.effective_models AS before,
       ARRAY(SELECT LOWER(JSONB_ARRAY_ELEMENTS_TEXT(mg.allowed_models)) ORDER BY 1) AS after
FROM pre_migration_snapshot s
JOIN downstreams d ON d.id = s.downstream_id
JOIN model_groups mg ON mg.id = d.model_group_id
WHERE s.effective_models IS DISTINCT FROM
      ARRAY(SELECT LOWER(JSONB_ARRAY_ELEMENTS_TEXT(mg.allowed_models)) ORDER BY 1);
```

**发布顺序**：新代码（阶段 1）**先上线**，迁移 SQL 后跑。新代码兼容"有组/无组"两种状态；老代码不认识新读路径。

**2.4 验收**

- `SELECT COUNT(*) FROM downstreams WHERE model_group_id IS NULL` = 0
- §2.3 对比查询 0 行
- §2.3（Codex）目录 diff：**每个在用 key 逐位无差异**
- 抽查 3 个 key：迁移前能用的模型仍能用，不能用的仍不能用（含一个 Codex key 实跑一次会话）
- 生成的组数量远小于 key 数；若接近 1:1，人工确认是否符合预期

### 3.3 阶段 3：停写 `model_allowlist`（数据仍在，可回滚）

后端：
- `admin.rs:2302` 移除写入分支，改为忽略 + warn（老客户端还在传时不报错）
- `admin.rs:2709` 从批量可改字段移除 `"model_allowlist"`
- `state.rs:8401` 同 `admin.rs:2302`
- **`state.rs:7230 apply_model_qualification`**：改成把资格审定结果写入**目标下游所绑分组**的 `allowed_models`（已拍板）。它是唯一的功能性写入，不许跳过。三条防外溢规则必须一起实现，否则单个下游的审定会改掉别人的权限：
  1. 下游未绑组 → 返回 `InvalidInput`，不要退回去写 `model_allowlist`，也不要自动建组。
  2. 所绑组是内置组（`all` / `basic` / `premium` / `deny-all`）→ 拒绝，错误信息提示"请先改绑到专属分组"。
  3. 所绑组被 **2 个及以上**下游引用（`SELECT COUNT(*) FROM downstreams WHERE model_group_id = $1`）→ 拒绝，同样提示先改绑。
  写入用 `UPDATE model_groups SET allowed_models = $2, updated_at = NOW() WHERE id = $1`，与下游快照的更新放在同一个事务里；组缓存（若阶段 1 加了）要一并失效。
- `admin.rs:1263 portal_model_is_allowed(allowlist, …)` 改用有效 allowlist
- `postgres.rs:1417-1433` **暂不动**，继续写 `downstream_model_allowlist` 保留回滚数据
- 新建下游 `model_group_id` 必填，未指定时落 **`deny-all`**（已拍板；不要 `basic`，绝不要 `all`）

前端：
- `Downstreams.vue:329-340` 删掉 `modelManagementMode` 的 `manual` 选项与整个手动分支（`:396`）
- `Downstreams.vue:77-81` 表格列只显示分组
- `Downstreams.vue:868-882` 删掉 mode 切换的两个 watch
- `frontend/tests/views/admin-ui.spec.ts:237` 断言 `formatModelList(row.model_allowlist)`，要跟着改
- `types/index.ts:192` 标 `@deprecated`
- 门户四个页面（`Overview.vue` / `QuotaDetails.vue` / `Playground.vue` / `Integration.vue`）消费的是 `portal_quota` 返回的 `model_allowlist` **字段名**，阶段 1 已让它返回有效清单，**前端不改**

运维入口：后台加"未绑组的下游"检查，或 `SELECT id, name FROM downstreams WHERE model_group_id IS NULL;`

验收：管理界面无法手工输入模型列表；PUT 传 `model_allowlist` 被忽略且有 warn、不报错；批量接口不接受该字段；门户页面显示正常；`scripts/redis_runtime_smoke.sh:360` 里的 fixture 跟着更新。

### 3.4 阶段 4：删列删表（阶段 3 稳定运行 ≥ 2 周后）

**必须先解决权限放大**：现在 `model_group_id = NULL` 回退白名单、空白名单 = 全放行；删掉白名单后 NULL 组会直接变"无限制"。

坑在于 `model_list_allows` 把**空列表当放行全部**（`usage.rs:275`），所以"什么都不许"不能用空数组表达。已拍板用 sentinel 组 `deny-all`（阶段 0 已建），本阶段只改外键与列约束。被否掉的备选是"改解析层语义让空列表 = 拒绝全部"——语义更干净但要动 `model_list_allows` 所有调用方，回归面太大。

```sql
ALTER TABLE downstreams DROP CONSTRAINT fk_downstream_model_group;
ALTER TABLE downstreams
  ALTER COLUMN model_group_id SET DEFAULT 'deny-all',
  ALTER COLUMN model_group_id SET NOT NULL;
ALTER TABLE downstreams
  ADD CONSTRAINT fk_downstream_model_group
  FOREIGN KEY (model_group_id) REFERENCES model_groups(id) ON DELETE SET DEFAULT;
```

然后：

```sql
-- migrations/2026-XX-XX-drop-model-allowlist.sql
BEGIN;
DROP TABLE IF EXISTS downstream_model_allowlist;
COMMIT;
```

代码侧：`types.rs:1135` 删字段、`types.rs:1326-1355` `get_allowed_models` 简化为直接读组、`postgres.rs:188/226-238/1417-1433` 删读写、`postgres.rs:1930` 从 `SCHEMA_SQL` 删建表、`AppState::effective_model_allowlist` 去掉回退分支、`gateway-core/src/admin.rs:247` 改用组、`types_model_group_tests.rs` 的两个 `falls_back_to_model_allowlist_*` 用例删除。

**约 76 个测试文件构造 `DownstreamConfig` 时显式写了 `model_allowlist`**（其中 62 个已在用 `..Default::default()`）。删字段前先把这些统一改成 `..Default::default()`，再删字段，否则一次性编译错误会淹没真正的回归。

删表前 `pg_dump -t downstream_model_allowlist` 备份。

---

## 4. 开发任务（TDD，按顺序）

项目规矩：**先写失败的测试，看到它因"功能不存在"而失败，再写实现**。每个任务都是一轮 RED → GREEN → REFACTOR。

### 阶段 0

| # | 任务 | 验证 |
|---|---|---|
| T0-1 | `migrations/2026-09-06-fix-model-group-seeds.sql`：建 `deny-all`、改 `basic`/`premium` 真实模型 | ✅ 生产库已应用且幂等重跑不变；`GET /admin/models` 复核 24 个真实 slug，清单全部命中（`GLM-5.2`→`glm-5.2` 按原始拼写）；四个组内容符合预期 |
| T0-2 | 同步 `postgres.rs` `SCHEMA_SQL` 种子 | ✅ SCHEMA_SQL 种子与 migration 一致；顺带修复 FK/索引块位于 `model_groups` 建表之前导致的 SCHEMA 初始化失败，并幂等清理残留 `model_group_id` 引用后再建 FK；`tests/model_groups_migration.rs` 断言 4 种子组 + deny-all 哨兵 + 真实模型（6 用例绿） |
| T0-3 | 备份 + Codex 目录快照 | ✅ `pg_dump` 至 `~/backups/chat-responses-codex/20260906/`（196 行）；test/wsl 两 key `format=codex` 目录快照 332/95 字节落盘；门户 basic key 实跑 `deepseek-v4-flash` 会话 HTTP 200 |

**阶段 0 附注**：修复测试基建 `tests/common/oidc.rs`（reset 漏清 `downstreams` 表导致跨测试残留、HTTP 分组测试命中旧副本）；`tests/downstream_model_groups.rs` invalid-group 语义更新为「真实组→删除→FK SET NULL→回退 allowlist」（FK 禁止绑定不存在分组）；10/10 用例绿。

### 阶段 1

| # | 任务 | RED 测试 |
|---|---|---|
| T1 | `AppState::effective_model_allowlist` | 单测：无组回退白名单；有组返回组内容；组解析失败返回 Err；无 portal store 回退白名单 |
| T2 | `gateway.rs:5570` / `:3282` / `state.rs:6220` 改调用 T1 | 既有测试保持绿；补一条"三处对同一 key 返回同一 allowlist"的一致性测试 |
| T3 | **Codex 目录走分组**（`gateway.rs:3056`） | `tests/downstream_model_groups.rs` 加：绑组 key 请求 `/v1/models?format=codex`，`.models[].slug` 等于组内容 ∩ upstream 暴露；组解析失败返回 500 |
| T4 | `portal_quota` / `portal_model_probe` 走分组 | `tests/portal_api.rs`：绑组 key 的 `model_allowlist` 字段返回组内容；组查不到时返回空数组 + 不 500 |
| T5 | `compute_model_stats` / `compute_portal_model_context_limits` 走分组 | `tests/portal_api.rs` 或新文件：统计只含组内模型 |
| T6 | `log_queries.rs` + `postgres.rs` 两套 usage summary 走分组 | `tests/troubleshooting.rs` / `tests/postgres_roundtrip.rs`：`total_models` / `active_models` 按组算 |
| T7 | `state.rs:6145/6298/6733` 走分组 + 批量查询消 N+1 | `tests/capability_probe.rs`：绑组下游只对组内模型排探测；`/admin/models?scope=visible` 只列组内模型 |

### 阶段 2

| # | 任务 | 验证 |
|---|---|---|
| T8 | 迁移 SQL + 幂等性 | `tests/model_groups_migration.rs` 风格的 DB 测试（`OIDC_TEST_DATABASE_URL`）：造 3 个下游（空白名单 / 相同白名单两个 / 独立白名单），跑迁移，断言组数 = 2 + `all`、`model_group_id` 全非空、有效模型集合逐位不变；重复跑一次结果不变 |
| T9 | 迁移前后 Codex 目录 diff 脚本 | `scripts/` 下加一个脚本，遍历在用 key 拉 `/v1/models?format=codex` 存快照并 diff |

### 阶段 3

| # | 任务 | RED 测试 |
|---|---|---|
| T10 | 停写：PUT / 批量 / `update_downstream_by_id` | `tests/admin_downstreams.rs`：传 `model_allowlist` 返回 200 但值不变 |
| T11 | `apply_model_qualification` 改写分组 | `tests/admin_upstreams.rs`：审定后目标下游所绑组的 `allowed_models` = 保留模型集合、`model_allowlist` 不变；未绑组 / 内置组 / 组被多下游引用三种情况各返回错误且**不修改任何组** |
| T12 | 新建下游必须带组 | `tests/admin_downstreams.rs`：不传 `model_group_id` 时落 `deny-all`，用该 key 请求任何模型 403 |
| T13 | 前端下线手动模式 | `frontend/tests/views/admin-ui.spec.ts` 改断言：不再出现手动白名单控件 |

### 阶段 4

| # | 任务 | RED 测试 |
|---|---|---|
| T14 | 测试夹具统一 `..Default::default()` | 全量 `rtk cargo test` 绿 |
| T15 | `deny-all` 兜底 + 外键 `SET DEFAULT` | DB 测试：删掉某组后，原绑定下游落到 `deny-all`，请求任何模型 403（**不是**放行全部） |
| T16 | 删字段、删表、删回退分支 | 全量测试绿；`tests/postgres_roundtrip.rs` 不再引用该表 |

---

## 5. 测试要求

```bash
rtk cargo test                 # 全量；DB 用例需 OIDC_TEST_DATABASE_URL
rtk cargo clippy               # 不许新增 warning
cd frontend && rtk vitest run   # 前端
```

重点回归套件：`tests/downstream_model_groups.rs`、`tests/gateway/model_permission_validation.rs`、`tests/portal_api.rs`、`tests/downstream_quota.rs`、`tests/troubleshooting.rs`、`tests/capability_probe.rs`、`tests/postgres_roundtrip.rs`、`tests/model_groups_migration.rs`。

线上验收还要跑 §2.3 的 Codex 端到端三条命令。

---

## 6. 风险与应对

| 风险 | 影响 | 应对 |
|---|---|---|
| Codex 目录与实际权限不一致 | Codex 侧启动即报错或选到必然 403 的模型 | 阶段 1 T3 优先修；每步做 §2.3 的 diff |
| `basic` / `premium` 是占位模型 | 选中即封死所有真实模型；绑定级闸门已在用 | 阶段 0 改种子数据，**两处都改**（migration 修既有库、`SCHEMA_SQL` 修新库） |
| 审定写入共享组 | 单个下游的审定改掉其他下游权限 | T11 的三条防外溢规则：未绑组 / 内置组 / 引用数 > 1 全部拒绝 |
| 组查询从 3 处扩到 12 处 | 高频接口变慢、循环内 N+1 | `state.rs` 三处循环先批量查；压测 P99；必要时 10–30s TTL 缓存 + 写时失效 |
| 迁移把不同拼写合并 | 组内容与原白名单字面不同 | 哈希按小写取、值留原拼写；用 §2.3 校验查询兜底 |
| `apply_model_qualification` 仍写白名单 | 阶段 4 删字段后功能静默失效 | T11 必须做，不许跳 |
| `SCHEMA_SQL` 改成 upsert | 每次启动覆盖运维手工调整的组内容 | 保持 `ON CONFLICT DO NOTHING` |
| 阶段 4 后 NULL 组 = 无限制 | **权限放大** | 先落 `deny-all` + `SET DEFAULT`（`model_list_allows` 空列表 = 放行，不能用空数组表达拒绝） |
| `auto-xxx` 组过多 | 运维界面混乱 | 迁移后人工合并/改名；提供"未被引用的组"清理查询 |
| `get_key_allowed_models` 未按 user 收敛 | 多用户绑同一 downstream 时命中行不确定 | 本方案不改，单独记 issue |
| 76 个测试文件显式写该字段 | 删字段时编译错误淹没真回归 | T14 先统一 `..Default::default()` |

---

## 7. 执行顺序

1. 阶段 0：T0-1/T0-2/T0-3（migration + `SCHEMA_SQL` 两处种子、建 `deny-all`、备份、Codex 目录快照）
2. 阶段 1：T1–T7（纯修 bug，独立发布，本身就有价值）
3. 阶段 2：新代码上线**之后**跑迁移 SQL（T8–T9），带前后对比证据
4. 观察 1 周
5. 阶段 3：T10–T13（停写 + 前端下线）
6. 观察 2 周
7. 阶段 4：T14–T16（改外键指向 `deny-all` + 删表删字段）

---

## 附录 A：对外部草案的修正

原草案 `docs/investigations/model-allowlist-deprecation-plan.md` 已按要求删除，此处保留其四处错误的修正记录，避免重犯：

1. 说 `portal_user_downstreams.model_group_id`"网关完全不强制，只在 `admin.rs:3490` 回显"。**错**：`gateway.rs:5664` 经 `get_key_allowed_models`（`portal_store.rs:1153`）在强制，且 NULL 时 fail-closed 落 `basic`。
2. 说 `portal.rs:255` 等"8 处读路径"不看组，把 `available_models_for_downstream` 也算进去了。**错**：`state.rs:6220` 已经看组；真正漏的是 `postgres.rs:637/674` 的 SQL 版 usage summary，草案没提。
3. 说"迁移前必须确认 `model_case_insensitive_matching`，否则 `LOWER()` 会改变匹配行为"。**错**：`portal_model_is_allowed` 无条件小写化，白名单匹配永远大小写不敏感，与该开关无关。
4. 建议"组被删除时 `SET DEFAULT 'basic'`"。**不足**：`basic` 是占位数据（阶段 0 先修）；且真正的兜底需要 sentinel 组，因为 `model_list_allows` 把空列表当放行全部。

草案漏掉的两处写路径：`state.rs:7230 apply_model_qualification`（功能写入）、以及 76 个测试文件的夹具改造工作量。
