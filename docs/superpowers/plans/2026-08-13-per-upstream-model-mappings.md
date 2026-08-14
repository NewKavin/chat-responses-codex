# 方案：按上游隔离的模型映射（Part B-3，承接并收口 B-2 路由侧遗留）

日期：2026-08-13
状态：已完成
前置：`2026-08-13-route-exhaustion-self-healing-and-model-alias-unification.md` 的 Part B-1（canonical 归一，`8894959`）与 B-2（全局别名注册表，`aac6e1f` + 前端 `7917bf0`）已合入 main。
废弃：`docs/model-mappings-b1-implementation-plan.md`（外部模型起草，技术栈假设与本仓库不符，见下文评审记录；本文档取代之）。

---

## 一、需求（用户原话归纳）

1. 管理页要能表达「**上游账号 + 上游模型名称 → 下游模型名称**」的三元组映射，
   平铺表格展示这三列，而不是现在的全局 canonical/aliases 规则视图。
2. 添加映射时：先选上游账号，再选模型；**模型下拉的数据源必须是该上游已配置的
   `supported_models`**（`src/state/types.rs:426`，即 Upstreams 页「手动输入或点击
   获取模型」维护的清单，`frontend/src/views/admin/Upstreams.vue:248`），
   **不得**现场调用上游 `/v1/models` 拉全量列表。
3. 不同上游的同名模型可以映射成不同的下游名称（例：上游 A 的 `gpt-4` →
   `gpt-4-premium`，上游 B 的 `gpt-4` → `gpt-4-standard`），互不影响。

第 3 点是全局规则（B-2 `ModelAliasRule`）**在数学上无法表达的**：全局规则以模型名
为键，同一个 alias 出现在两条规则里会被注册表校验直接拒绝
（`src/state/model_identity.rs:167-174`）。所以这不是 UI 改版，而是需要新的
按上游数据模型。

## 二、现状与差距（已核实代码）

| # | 现状 | 证据 | 差距 |
|---|------|------|------|
| 1 | B-2 全局规则已上线：`PersistedState.model_aliases`、注册表热更新、Admin API `GET/PUT /admin/model-aliases` | `src/state/types.rs:859`、`src/state.rs:2938-2951`、`frontend/src/api/admin.ts:430-433` | 规则是全局的，无上游维度 |
| 2 | 请求入口把下游模型名做「全局 alias → canonical」归一 | `src/server/gateway.rs:4383-4390`（`normalized_model`） | 归一结果只用于**正向**匹配 |
| 3 | 路由匹配只有大小写折叠，无别名反向展开：请求 `deepseek-v3` 命中不了只声明 `deepseek-chat` 的上游 | `src/state/normalize.rs:195/208/383`（`supports_model_with` / `resolved_model_name_with` / `keys_for_model_with` 仅 case-fold）；`matches_canonical` / `all_spellings_for_canonical` 在生产代码零调用 | 主方案 B-2 设计第 2 条（反向解析）**未实现**，测试要求第 3 条不满足 |
| 4 | 模型列表显示侧已做 alias 展示与 canonical 去重 | `src/state.rs:5147-5202` | 需叠加按上游映射后的对外名称 |
| 5 | 前端页面是「全局映射规则」+「上游模型浏览器（快速添加）」两栏 | `frontend/src/views/admin/ModelAliases.vue`（路由 `/admin/model-aliases`，`router/index.ts:79-81`） | 与用户要的三列平铺视图不符；快速添加把上游模型名填进 `canonical`（下游名），方向与用户心智相反 |

**设计取舍**：B-2 遗留的「全局规则路由反向展开」不再单独实现——按上游映射天然
覆盖该需求（给只声明 `deepseek-chat` 的上游配一条 `deepseek-chat → deepseek-v3`
即可路由），且语义更精确。全局规则保留现有职责：入口归一 + 列表显示拼写控制。

## 三、设计

### 3.1 数据模型（放进 UpstreamConfig，不建新表）

本仓库持久化是 `PersistedState` 整体快照（file / postgres / redis 三后端，
`src/state/file_store.rs` / `postgres.rs` / `redis_runtime.rs`），**没有** sqlx、
migrations、关系表。映射直接挂在上游配置上：

```rust
// src/state/types.rs
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamModelMapping {
    /// 该上游 supported_models / api_key_models 中的原拼写（发往上游用）
    pub upstream_model: String,
    /// 下游可见与请求用的名称
    pub downstream_model: String,
}

// UpstreamConfig 增加：
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub model_mappings: Vec<UpstreamModelMapping>,
```

好处（对比外部草案的 SQL 表方案）：
- 持久化、三后端往返、快照热重建、导入导出全部复用现有链路，零迁移；
- 删除上游 = 映射天然级联删除（草案里的 `ON DELETE CASCADE`、UUID、索引、
  迁移脚本在本架构下都是多余物）；
- 老配置文件无该字段 → `serde(default)` 空数组，向后兼容免费。

### 3.2 校验（上游保存路径，admin.rs 既有校验点内）

对每个上游的 `model_mappings`，canonical 比较（`canonical_model_id`）：
1. `upstream_model`、`downstream_model` 非空（trim 后）；
2. 组内 `upstream_model` 唯一（同一上游模型只能映射一次）；
3. 组内 `downstream_model` 唯一（否则同名两条无法反向定位）；
4. `downstream_model` 不得与**本上游未映射**的其它模型同名（canonical），
   否则该名字有两个来源，路由歧义；
5. `downstream_model` 不得命中任何全局规则的 alias（入口会把它再归一成别的
   canonical，映射将永远匹配不上；报错提示改用该规则的 canonical 或删规则）；
6. `upstream_model` 允许不在当前 `supported_models` 里（模型同步可能暂时移除），
   保存放行但标记为「失效」，路由时跳过（见 3.3），UI 显示状态列。

跨上游的 `downstream_model` 相同是**特性**（多上游聚合到同一个下游名），放行。

### 3.3 解析顺序（请求路径）

对进入的下游模型名 R（`gateway.rs:4383` 已产出 `normalized_model` =
全局 alias → canonical）：对每个候选上游 U：

1. **按上游映射优先**：U.model_mappings 中存在 `downstream_model ≡ normalized_model`
   （canonical 比较）→ 命中；`runtime_model_slug` 取该条 `upstream_model` 在
   `route_models()` 中的**原拼写**（在 `supported_models` ∪ `api_key_models`
   中 canonical 查找；找不到 = 失效映射，跳过该条继续）。
2. **回退现有匹配**：走 `supports_model_with` / `resolved_model_name_with`
   的 case-fold 逻辑，但要**排除已被映射占用的 upstream_model**
   （rename 语义：上游 A 的 `gpt-4` 映射成 `gpt-4-premium` 后，下游请求
   `gpt-4` 不应再命中上游 A；未配映射的上游 B 不受影响）。
3. 命中后所有上游侧检查沿用解析出的原拼写：`keys_for_model_with`、
   `is_premium_model_request`、`ModelContextConfig.slug` 查找、
   `RouteHealthKey.runtime_model_slug`、发往上游的 payload `model` 字段。
4. 下游侧键（usage / 配额 / affinity / 日志 model 字段）继续用
   `normalized_model`（下游名），与现状一致。

实现位置建议：`normalize.rs` 的 `supports_model_with` / `resolved_model_name_with` /
`keys_for_model_with` 增加 mappings 感知（或新增 `*_with_mappings` 变体），
调用点对照 B-1 commit `8894959` 的接线清单逐一过（该 commit 就是在同一批缝隙
里加的 case_insensitive 参数，改造面完全重合）。

### 3.4 模型列表（/v1/models 标准与 codex 两种格式）

每上游对外有效集合 = { 映射的 `downstream_model` } ∪ { 未映射模型原拼写 }，
再进入现有的全局 alias 显示 + canonical 去重管道（`state.rs:5147-5202`、
`list_models_codex_format`）。被映射占用的 `upstream_model` 原名**不再出现**
（除非其它上游未映射地声明了它）。

### 3.5 Admin API

**不新增端点**。`model_mappings` 是 `UpstreamConfig` 的一部分，现有上游
CRUD（`GET /admin/upstreams`、上游更新端点）自动携带；前端读列表、改单个
上游即可。外部草案的三个新端点（`/upstreams/{id}/model-mappings` 等）无必要。

### 3.6 前端（ModelAliases.vue 改版，路由不变）

页面改为两个 tab：

**Tab 1「模型映射」（默认）**——用户要的视图：
- 平铺表格：`上游账号 | 上游模型名称 | 下游模型名称 | 状态 | 操作(编辑/删除)`，
  数据 = `getUpstreams()` 结果按 `model_mappings` 展开（无需新 API）；
  顶部支持按上游筛选 + 模型名搜索；「状态」列标记失效映射（3.2 第 6 条）。
- 「添加映射」对话框三步：选上游（下拉）→ 选上游模型（**数据源 = 该上游
  `supported_models` ∪ `api_key_models[].supported_models` 去重**，已映射的
  条目禁用；绝不调上游 /models）→ 输入下游名称。
- 编辑：锁定上游与上游模型，只改下游名称；删除：确认后从该上游
  `model_mappings` 移除。
- 保存沿用 Upstreams.vue 既有的上游更新调用（整个 UpstreamConfig 提交）。
- 前端类型：`frontend/src/types` 的 `UpstreamConfig` 增加 `model_mappings`。

**Tab 2「全局规则」**：现有 ModelAliases 内容原样保留（含上游模型浏览器），
页头描述改为「跨上游的拼写归并与显示控制；按上游改名请用『模型映射』tab」。

## 四、开发任务

| # | 任务 | commit | 涉及文件 |
|---|------|--------|---------|
| M1 | `UpstreamModelMapping` 类型 + `UpstreamConfig.model_mappings` 字段 + 校验（3.2 全部 6 条，含单测） | `82be30c` | `src/state/types.rs`、`src/server/admin.rs`（上游保存校验点） |
| M2 | 解析与路由集成（3.3）：mappings 优先命中、原名排除、失效跳过；上游侧检查全部走原拼写 | `6c7f02a` | `src/state/normalize.rs`、`src/state.rs`、对照 `8894959` 接线清单 |
| M3 | 模型列表集成（3.4）：两种格式的对外集合替换 | `b6309f3` | `src/state.rs`、`src/server/gateway.rs`（codex 格式处） |
| M4 | 前端改版（3.6）：双 tab、平铺表格、三步添加对话框、类型定义 | `4f81f19` | `frontend/src/views/admin/ModelAliases.vue`、`frontend/src/types` |
| M5 | 文档：管理说明补「模型映射 vs 全局规则」一节；主方案文档 B-2 遗留项标注「由本方案承接」 | `2e1bcba` | `docs/deployment-model-aliases-ui.md`、两份 plan 文档回填 |

每任务独立 commit，测试先行，`rtk cargo test` / `rtk tsc` 全绿后提交；
完成后把 commit 号回填到本文档。

## 五、测试要求

后端（复用 `tests/gateway/` mock 上游设施 + `tests/postgres_roundtrip.rs`）：

1. **隔离映射（核心场景）**：上游 A `gpt-4 → gpt-4-premium`，上游 B
   `gpt-4 → gpt-4-standard`，上游 C 未映射声明 `gpt-4`：
   - 请求 `gpt-4-premium` 只路由 A，mock 断言 payload `model == "gpt-4"`；
   - 请求 `gpt-4-standard` 只路由 B；
   - 请求 `gpt-4` 只命中 C（A/B 的原名已被映射占用）；
   - `/v1/models` 两种格式含 `gpt-4-premium` / `gpt-4-standard` / `gpt-4`（来自 C），
     不重复、不出现 A/B 名下的 `gpt-4`。
2. **大小写与全局规则叠加**：映射 `DeepSeek-Chat → deepseek-v3`，请求
   `DEEPSEEK-V3` 命中且 payload 用原拼写 `DeepSeek-Chat`；全局 alias 命中后的
   canonical 再进按上游映射（顺序 3.3.1）。
3. **失效映射**：`upstream_model` 不在该上游任何模型清单 → 路由跳过该条、
   不 panic、日志可见；恢复 supported_models 后无需改配置即重新生效。
4. **校验**：3.2 的 2/3/4/5 各一条拒绝用例，错误消息含具体冲突名。
5. **持久化**：`model_mappings` 经 file JSON 与 postgres 往返不丢；旧配置
   无字段可加载（serde default）。
6. **键归属**：用量/配额记下游名（normalized），premium 判定用上游原拼写
   （A 的 `premium_models: ["gpt-4"]` 时请求 `gpt-4-premium` 计 premium）。
7. **回归**：`model_identity` 全部既有测试、B-2 列表显示用例、
   `codex_subagent_base_model` 相关用例全绿。

前端：`rtk tsc` 与 `npm run build` 通过；手测清单——添加对话框模型下拉只含所选
上游已配置模型且已映射项禁用；表格三列平铺可筛选；编辑锁定前两列；删除生效。

## 六、对外部草案（docs/model-mappings-b1-implementation-plan.md）的评审记录

方向正确（按上游三元组、TC-1~TC-10 用例意图大多可保留，已吸收进第五节），
但落地方案与仓库不符，不可执行：

1. 假设 sqlx + migrations + PostgreSQL 关系表（UUID/CASCADE/索引）——仓库无
   sqlx 依赖、无 migrations 目录，持久化是三后端整体快照；
2. 假设 `src/models/`、`src/db/`、`src/routing/model_resolver.rs` 目录结构——
   均不存在，真实缝隙在 `state/normalize.rs`、`state.rs`、`server/gateway.rs`；
3. 新增 3 个专用 API 端点——上游 CRUD 已整体携带配置，无必要；
4. 完全未处理与已上线 B-1/B-2 的叠加语义（解析顺序、原名遮蔽、列表去重、
   usage/premium 键归属）——这是本功能真正的难点；
5. 「保留旧 API 3 个版本」「sqlx migrate run」等章节为模板残留，不适用；
6. 文件名 "B-1" 与主方案 Part B-1（canonical 归一）撞名，易混淆。

## 七、上线后评审遗留（M6，待开发）

2026-08-14 对 M1-M5 实现的评审结论：五个任务与本方案一致，测试齐全（评审记录见当日会话）。
唯一实质缺口：**3.2 规则 5 的校验是单向的**——

- 上游保存方向已拦截：insert（`src/state.rs:4825-4833`）与 PATCH 合并路径
  （`src/state/freekey_sync.rs:735-741`）都会调 `validate_model_mappings_against_aliases`；
- 反方向缺失：`update_model_aliases`（`src/state.rs:2933-2959`）保存全局规则时
  **不检查既有上游映射**。先配映射 `gpt-4 → gpt-4-premium`，再在「全局规则」tab
  加一条 `aliases: ["gpt-4-premium"]` 可以保存成功；此后入口归一
  （`gateway.rs:4383`）会把请求改写成该规则的 canonical，映射永远匹配不上，
  **静默失效**且无任何报错。

### M6 任务

`update_model_aliases` 在 `ModelAliasRegistry::from_rules` 成功后、持久化前，
遍历当前全部上游执行 `validate_model_mappings_against_aliases(&new_registry)`，
任一冲突即拒绝整次保存；错误消息含：冲突上游名、映射的 `downstream_model`、
规则的 canonical，并提示「删除该映射或改用其它 alias」。

测试：先存映射再存冲突全局规则 → 拒绝且消息含三要素；不冲突的规则正常保存；
先存全局规则再存冲突映射 → 既有方向回归（仍拒绝）；`rtk cargo test alias` 全绿。
