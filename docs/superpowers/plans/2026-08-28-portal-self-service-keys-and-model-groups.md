# 管理员模型分组 + 下游用户自助创建 Key（第二阶段）

- 日期：2026-08-28
- 状态：待开发（交接给其他模型实现）
- **前置依赖（硬依赖，不可并行）**：`docs/superpowers/plans/2026-08-28-portal-oauth-login.md` 的 T1–T5 必须先全部完成。没有可信的"这个请求是哪个人发的"（服务端 session + `portal_users` + `portal_user_downstreams`），自助建 key 就是在造无主资源，**且无法做人维度的容量约束**（见 §3.3）。
- 已落地的相关能力（本方案在其之上叠加，不推翻）：C7 下游按模型分组的并发闸门（commit `a47f2373` / `d3fae88a` / `6b95ca0e` / `139a51b1`）。

## 1. 需求

1. **管理员配置模型分组** —— 分组是全局的、可维护的实体，不是散落在每把 key 上的副本。
2. **下游用户自助创建 key** —— OAuth 登录后自己建、自己看、自己轮换/停用，不用找管理员。
3. **按分组创建** —— 用户建 key 时选一个或多个管理员开放的分组，key 的模型准入与并发上限由分组决定，用户不能自己抬高。

## 2. 现状与缺口

### 2.1 现状

| 能力 | 现状 | 位置 |
| --- | --- | --- |
| 一个 Portal 用户能有几把 key | **恰好一把**，且等于他绑定的那条 downstream | `src/server/portal.rs:528-553` `portal_get_key` 直接返回 `downstream.plaintext_key` |
| 用户能做的 key 操作 | 只有「看」和「轮换」 | `/api/portal/key`、`/api/portal/key/rotate`（`src/server/gateway.rs:2558-2559`） |
| key 的载体 | 一条 `DownstreamConfig` = 一把 key，配额/并发/计费/IP 白名单/过期全挂在上面 | `src/state/types.rs:940-982` |
| 模型准入 | `model_allowlist: Vec<String>` | `portal_model_is_allowed`（`src/state/usage.rs:247-262`），调用点 `src/server/gateway.rs:3140`、`:5334` |
| 分组并发 | C7 已落地：`model_concurrency_groups: Vec<ModelConcurrencyGroup { name, patterns(serde `match`), max_concurrency }>` | `src/state/types.rs:981`、`:1150-1155`；校验 `:1074`；glob `glob_match_star` `:1161` |
| 批量改字段 | 已有 `POST /api/admin/downstreams/batch-update` | `src/server/gateway.rs:2491` |

### 2.2 缺口

1. **分组没有全局注册表。** C7 的 `model_concurrency_groups` 是**每把 key 各存一份副本**。管理员要给 50 把 key 配同一套分组，只能用 batch-update 硬推 50 份；改一次组就要再推一次，且没有任何机制能回答"哪些 key 用了 deepseek 组"。
2. **没有"用户拥有 key"的主体关系。**（第一阶段补 `portal_user_downstreams`，本阶段依赖它。）
3. **没有自助创建入口**，也没有任何"一个人最多能建多少容量"的约束——这是本方案里**最危险**的一处，见 §3.3。
4. **`model_allowlist` 为空 = 放行全部模型**（`src/state/usage.rs:248-250`）。自助 key 如果物化时漏写 allowlist，就等于给了全模型权限。这是必须在测试里钉死的一条。

## 3. 架构决策

> 这三条是本方案的骨架，实施模型**不要**自行改换方案；要改先回来改文档。

### 3.1 一把 key = 一条 `downstreams` 记录

**决策**：自助创建一把 key，就是创建一条新的 `DownstreamConfig` + 一条 `portal_user_downstreams` 绑定。

**理由**：配额窗口、并发闸门、计费、用量日志、IP 白名单、过期——全部已经以 `downstream.id` 为键实现。复用它，网关热路径一行不用改。

**反面（明确禁止）**：不要新造"一条 downstream 挂多把 key"的模型。那会让 gateway 热路径上**所有** per-key 计数（`downstream_runtime` 并发表、配额窗口、token 窗口、用量日志归属）全部要换键，是与收益完全不成比例的改造。

### 3.2 全局注册表 + 每 key 物化副本

**决策**：新增全局 `model_groups` 注册表（管理员管）；key 上**保留** C7 已有的 `model_concurrency_groups` 字段作为**生效副本**；key 另存 `model_group_refs: Vec<String>`（引用的组名）。保存 key 时由服务端从注册表**物化**出 `model_allowlist` 与 `model_concurrency_groups`。

**理由**：

- 网关热路径（`try_reserve_downstream_concurrency`、`portal_model_is_allowed`）**零改动**——它们继续只读 key 上的物化副本，不查注册表、不加锁、不引入新的一致性问题；
- 旧的、管理员手工配置的 key（`model_group_refs` 为空）**零影响**，行为逐字节不变；
- 管理员改组之后，用一个显式的"重新物化"动作批量刷新引用它的 key，**变更是可见、可审计、可回滚的**，而不是热路径上悄悄变语义。

**明确禁止**：不要在网关热路径上查注册表。那会把一个每请求都要走的判断变成需要跨表读 + 缓存失效的路径。

**物化规则**（必须精确实现）：

- `model_allowlist` = 所选各组 `patterns` 的**并集**。**绝不允许为空**——空等于放行全部模型（`src/state/usage.rs:248-250`）。若并集为空则拒绝创建，返回 400。
- `model_concurrency_groups` = 每个所选组一条 `{ name, match: patterns, max_concurrency }`，`max_concurrency` 取组的 `default_max_concurrency`（用户可**调低**，不可调高）。
- 组之间 `patterns` 有重叠时，按 `model_group_refs` 的**顺序**物化——C7 的匹配就是"先匹配先生效"（`src/state/types.rs` 的 doc comment 已写明），顺序即优先级，要在 API 文档里说清楚。
- 全局 `max_concurrency`（兜底）= 各组上限之和与管理员设定的 `per_user_max_concurrency` 中的较小值。

### 3.3 容量必须按「人」聚合，不能只按 key（**本方案最关键的一条**）

**问题**：C7 的组上限是 **per key** 的。用户自助建 10 把 key、每把都选 deepseek 组（上限 28），就有 280 个并发能穿过下游闸门去抢**真实只有 28** 的上游容量。剩下的全部堆到上游本地闸门/排队，等于用户点几下鼠标就能把整个网关的 deepseek 容量拖垮。**这不是配置失误，是自助功能天然打开的放大器——必须在设计层堵死。**

**决策**：并发闸门的计数键从 `downstream.id` 扩展为 **`scope`**：

- 自助创建的 key（`owner_portal_user_id.is_some()`）⇒ `scope = owner_portal_user_id`，**同一个人名下所有 key 共享一份组预算**；
- 管理员创建的 key（`owner_portal_user_id.is_none()`）⇒ `scope = downstream.id`，**行为与现在完全一致**。

也就是说 C7 的计数键 `(downstream.id, group)` 变成 `(scope, group)`，`scope` 默认取 `downstream.id`。这是一处**很小**的改动，却是自助功能能否安全上线的前提。

**同时**在组上设两道人维度的闸：`per_user_max_keys`（一个人在这个组里最多建几把 key）和 `per_user_max_concurrency`（一个人在这个组里的并发总额，即上面 scope 预算的值）。

### 3.4 数据面零影响（业务连续性的基石）

**本方案的全部改动都在控制面。** 网关数据面（`/v1/chat/completions`、`/v1/responses` 等）对一把 key 的处理链路——鉴权、`model_allowlist` 判定、配额、并发闸门、上游选路——**读的仍然只是 `DownstreamConfig` 上的物化字段**，不查 `model_groups`、不查 `portal_*` 任何一张表、不感知这把 key 是管理员建的还是用户自助建的。

这条必须在实施时守住，因为它同时是三件事的保证：

1. **存量 key 零影响**：`model_group_refs` 为空、`owner_portal_user_id` 为 `None` 的老 key，行为逐字节不变；
2. **回滚安全**：即使把 Portal/OAuth/自助功能整体关掉甚至回滚代码，已经建出来的 key **仍然是普通 downstream，继续可用**——用户不会因为控制面回滚而断服务；
3. **故障隔离**：Postgres 挂了、OAuth provider 挂了、Portal 挂了，**推理流量不受影响**。

**验收要求**：要有一条测试，关闭 `PORTAL_SELF_SERVICE_KEYS_ENABLED`、断开 portal 存储之后，一把自助创建出来的 key 仍能正常完成一次推理请求。

### 3.5 存量用户的认领流程（**Day-1 连续性，不可省略**）

**问题**：第一阶段的设计是"OAuth 登录成功但没有 downstream 绑定 ⇒ `403 portal_access_not_granted`"。这条规则本身是对的（不能让外部身份自动获得资源），但如果只有这一条，**上线当天所有存量用户全部被挡在门外**——他们手里有 key、有工号，却没有任何 `portal_user_downstreams` 记录，只能一个个等管理员手工绑定。这是不可接受的连续性断裂。

**决策：提供三条建立绑定的路径，按优先级实现。**

**(a) 用户自助认领（必做）** —— 首次 OAuth 登录且无绑定时，不要直接甩一个 403 错误页，而是引导到**认领页**：用户输入现有的「工号 + 密钥」，服务端复用现有校验逻辑（`state.downstream_for_secret(&key)` + `downstream.id == employee_id`，见 `src/server/portal.rs:52-76`）确认他确实持有这把 key，验证通过后建立 `portal_user_downstreams` 绑定并标记 `is_default = true`。

- 认领是**证明持有**，不是登录，所以必须**要求已有有效 OAuth session**才能调用，防止变成一个新的爆破面；
- 必须限速（见 §3.7）并记审计事件；
- 同一条 downstream 已被别人认领过 ⇒ 拒绝，并告警（这是凭据泄漏的信号，不要静默覆盖绑定）。

**(b) 管理员预绑定 / 批量导入（必做）** —— 管理员按 `email → downstream_id` 的映射批量建立绑定，用户首次登录即可直接进入。适合已有花名册的场景。走 §5.2 的用户管理接口。

**(c) 自动开通（可选，默认关）** —— 第一阶段配置项已预留 `PORTAL_OAUTH_AUTO_PROVISION`（默认 `false`）。若打开，首次登录的合法身份自动获得一把按指定默认组创建的新 key。**默认保持关闭**，打开前必须先配好 `per_user_max_keys` / `per_user_max_concurrency`，否则等于把 §3.3 的容量放大器接到了 IdP 上。

### 3.6 容量账本：上游真实容量 vs 下游已分配（**并发限制的完整闭环**）

§3.3 挡住了"单个用户无限放大"，但挡不住"100 个用户每人合法地拿 4 并发"。管理员需要一个能回答**"我到底超卖了多少"**的视图，否则分组上限只是拍脑袋的数字。

**上游侧容量**（可从现有配置算出）：对每个模型组，遍历启用的上游，用 `glob_match_star` 把组的 `patterns` 与上游的 `supported_models`（`src/state/types.rs:740`）以及每个 key 的 `api_key_models[].supported_models`（`:864-869`）比对，命中的 **key 数 × 该上游 `max_concurrency`** 求和，即该组的真实上游容量。

**下游侧已分配**：`Σ(引用该组的各 scope 的组上限)`——按 §3.3 的 scope 聚合，自助 key 按人算一份，管理员 key 按 key 算一份。

**呈现**：管理员分组页每组显示 `上游容量 / 已分配 / 超卖倍数`，超卖时黄色警示但**不阻断**（超卖是合法策略——配合 C3 排队可以做统计复用），只是必须**可见**。

> 口径说明必须写进 UI 和文档：上游容量是**理论上限**，前提是聚合网关按 key 限流（已确认）且这些 key 未被其他用途占用。这是个规划参考值，不是硬保证。

### 3.7 滥用防护与生命周期

自助功能天然需要这两样，缺了会在上线后变成运维负担：

- **限速**：`POST /api/portal/keys`（创建）、`/keys/{id}/rotate`（轮换）、认领接口，都要有 per-user 与 per-IP 的速率限制。创建/认领建议每人每小时个位数。
- **有效期**：组的 `default_expires_in_days` 建议**非空**，自助 key 默认带过期时间，映射到 `DownstreamConfig.expires_at`（字段已存在）。到期前在 Portal 提示，到期后自动失效——避免无人认领的 key 无限堆积。
- **离职/停用**：Portal 用户被 disable 时，其名下所有自助 key **同步停用**（不是删除，保留审计与用量历史）。重新启用时**不自动恢复** key，需管理员显式操作——离职回收必须是确定性的。

## 4. 数据模型

### 4.1 新表 `model_groups`（Postgres migration，additive）

| 列 | 类型 | 说明 |
| --- | --- | --- |
| `name` | `TEXT PRIMARY KEY` | 组标识，`[a-z0-9_-]{1,64}`，创建后不可改（改名等于删旧建新） |
| `display_name` | `TEXT NOT NULL` | 展示名，可中文 |
| `description` | `TEXT NOT NULL DEFAULT ''` | 给用户看的说明（哪些模型、大致容量） |
| `patterns` | `JSONB NOT NULL` | 模型匹配模式数组，语义与 C7 的 `match` 完全一致，复用 `glob_match_star` |
| `self_service` | `BOOLEAN NOT NULL DEFAULT FALSE` | **默认 false**：不开放自助。管理员显式打开才出现在用户端 |
| `default_max_concurrency` | `INT NOT NULL` | 组的并发上限（同时也是 `per_user_max_concurrency` 的默认值） |
| `per_user_max_keys` | `INT NOT NULL DEFAULT 1` | 一个人在本组最多建几把 key |
| `per_user_max_concurrency` | `INT NOT NULL` | 一个人在本组的并发总额（§3.3 的 scope 预算） |
| `default_request_quota_window_hours` / `default_request_quota_requests` | `INT NULL` | 自助 key 的默认请求配额，映射到 `DownstreamConfig` 同名字段 |
| `default_daily_cost_limit_cents` | `BIGINT NULL` | 自助 key 的默认日成本上限 |
| `default_expires_in_days` | `INT NULL` | 自助 key 默认有效期；`NULL` = 不过期。**建议默认设一个值**，避免长期无人认领的 key 堆积 |
| `requires_approval` | `BOOLEAN NOT NULL DEFAULT FALSE` | 为 true 时自助创建进入待审批状态（见 §5.2） |
| `enabled` | `BOOLEAN NOT NULL DEFAULT TRUE` | 停用后不能再建新 key；**已建的 key 不受影响**（停组不等于断服务，要断服务用批量停用 key） |
| `created_at` / `updated_at` | `BIGINT NOT NULL` | |

文件兼容模式（无 `DATABASE_URL`）沿用第一阶段 §3.3 的同一决策：OAuth 相关端点返回 `503`。本阶段的**管理员**分组 CRUD 可以在文件模式工作（它只是配置），但**自助创建**端点必须同样返回 `503`，因为它依赖 session。

### 4.2 `DownstreamConfig` 新增字段（全部 `#[serde(default)]`，向后兼容）

```rust
/// 自助 key 的归属人；None = 管理员创建的传统 key，行为与改动前完全一致。
/// 同时是 §3.3 并发聚合的 scope 键。
#[serde(default, skip_serializing_if = "Option::is_none")]
pub owner_portal_user_id: Option<String>,

/// 引用的全局模型组名（有序，顺序即匹配优先级）。
/// 为空 = 传统 key，model_allowlist / model_concurrency_groups 由管理员手工维护。
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub model_group_refs: Vec<String>,

/// 物化时间戳，用于回答"这把 key 的副本是不是比组的定义旧"。
#[serde(default, skip_serializing_if = "Option::is_none")]
pub model_groups_materialized_at: Option<u64>,
```

**不要**新增 `self_service: bool`——`owner_portal_user_id.is_some()` 就是它，多一个字段就多一处会不一致的真相。

### 4.3 新表 `portal_user_group_quotas`（per-user 配额覆盖，§5.2）

| 列 | 类型 | 说明 |
| --- | --- | --- |
| `portal_user_id` | `UUID NOT NULL REFERENCES portal_users(id) ON DELETE CASCADE` | |
| `group_name` | `TEXT NOT NULL REFERENCES model_groups(name) ON DELETE CASCADE` | |
| `max_keys` | `INT NULL` | `NULL` = 回落组默认 |
| `max_concurrency` | `INT NULL` | `NULL` = 回落组默认；这是 §3.3 scope 预算的实际取值来源 |
| `note` | `TEXT NOT NULL DEFAULT ''` | 为什么给这个人提额，运维可追溯 |
| `updated_by` / `updated_at` | `TEXT` / `BIGINT NOT NULL` | |
| 主键 | `(portal_user_id, group_name)` | |

### 4.4 新表 `portal_audit_events`（§5.2 审计）

| 列 | 类型 | 说明 |
| --- | --- | --- |
| `id` | `BIGSERIAL PRIMARY KEY` | |
| `occurred_at` | `BIGINT NOT NULL` | |
| `actor_kind` | `TEXT NOT NULL` | `admin` / `portal_user` / `system` |
| `actor_id` | `TEXT NOT NULL` | |
| `action` | `TEXT NOT NULL` | `key.create` / `key.rotate` / `key.disable` / `key.delete` / `claim.success` / `claim.conflict` / `user.toggle` / `user.bind` / `user.unbind` / `quota.override` / `session.revoke` / `group.rematerialize` … |
| `target_kind` / `target_id` | `TEXT NOT NULL` | |
| `request_id` | `TEXT NULL` | 与网关日志对齐 |
| `detail` | `JSONB NULL` | **白名单字段，禁止写入任何明文密钥或 token** |

索引：`(occurred_at)`、`(actor_id, occurred_at)`、`(target_id, occurred_at)`。保留策略与清理任务写进 §S11 运维文档。

### 4.5 新表 `portal_key_requests`（仅当实现 `requires_approval` 时需要）

`id / portal_user_id / group_name / requested_at / status(pending|approved|rejected) / decided_by / decided_at / reason / created_downstream_id`。

## 5. HTTP API

错误体沿用项目既有 `{"error":{"message","code"}}` 形状。

### 5.1 管理员侧：模型分组

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| `GET` | `/api/admin/model-groups` | 列出全部组，带 `key_count`（引用该组的 key 数）和 `stale_key_count`（物化时间早于组 `updated_at` 的 key 数） |
| `POST` | `/api/admin/model-groups` | 创建 |
| `GET` | `/api/admin/model-groups/{name}` | 详情 |
| `PUT` | `/api/admin/model-groups/{name}` | 更新（partial merge，风格对齐 `admin_update_upstream`，`src/server/admin.rs:1883-1892`） |
| `DELETE` | `/api/admin/model-groups/{name}` | 删除；**被引用时拒绝**（409 + 引用它的 key id 列表），要先迁移或强制解引用 |
| `POST` | `/api/admin/model-groups/{name}/rematerialize` | 把组的最新定义重新物化到所有引用它的 key；返回逐 key 成功/失败，风格对齐 `/downstreams/batch-update`（`src/server/gateway.rs:2491`） |
| `GET` | `/api/admin/model-groups/{name}/keys` | 列出引用该组的 key（含归属人、是否 stale） |
| `GET` | `/api/admin/model-groups/{name}/capacity` | §3.6 容量账本：上游真实容量 / 下游已分配 / 超卖倍数 |

**`PUT` 不自动重新物化。** 改组只改注册表，`rematerialize` 是显式的第二步。理由：批量改上百把 key 的生效配置是高影响动作，必须让管理员看到影响面再点确认；同时 `stale_key_count` 让"改了没生效"这件事在 UI 上可见，不会变成静默的不一致。

### 5.2 管理员侧：Portal 用户管理

**这是管理员日常运维的主界面，不是附属功能**——用户的入职、离职、额度调整、异常处置都在这里。

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| `GET` | `/api/admin/portal-users` | 列表：身份（provider/email/display_name）、状态、最后登录、持有 key 数、各组并发占用。支持按状态/组/关键字筛选与分页 |
| `GET` | `/api/admin/portal-users/{id}` | 详情：身份列表、绑定的 downstream、名下每把 key（含归属组、用量、是否 stale）、活跃 session 数 |
| `POST` | `/api/admin/portal-users/{id}/toggle` | 启用/停用。**停用时同步停用其名下所有自助 key 并吊销全部 session**（§3.7）；启用**不自动恢复 key** |
| `POST` | `/api/admin/portal-users/{id}/bindings` | 绑定一条已有 downstream（§3.5(b) 管理员预绑定），body `{ "downstream_id": "...", "is_default": bool }` |
| `DELETE` | `/api/admin/portal-users/{id}/bindings/{downstream_id}` | 解绑。**解绑不删除 key**，只解除归属；要停服务请单独停用该 key |
| `POST` | `/api/admin/portal-users/bindings/batch` | 批量预绑定（`email → downstream_id` 映射数组），逐条返回成功/失败，风格对齐 `/downstreams/batch-update` |
| `PUT` | `/api/admin/portal-users/{id}/quota-overrides` | 针对**单个用户**覆盖某组的 `per_user_max_keys` / `per_user_max_concurrency`（VIP 或临时提额）。为空表示回落组默认值 |
| `POST` | `/api/admin/portal-users/{id}/sessions/revoke` | 吊销该用户全部 session（凭据泄漏应急） |
| `GET` | `/api/admin/portal-audit` | 审计事件查询：谁在什么时候建/轮换/停用/删除了哪把 key、管理员做了哪些用户操作。支持按用户/时间/事件类型筛选 |

**权限**：以上全部走现有管理员鉴权（`/api/admin/*` 既有的 JWT 校验），不引入新的权限模型。

**审计**：§5.2 与 §5.3 的所有**写操作**都必须落审计事件，字段含 `actor`（管理员 id 或 portal_user_id）、`action`、`target`、`timestamp`、`request_id`，**不含任何明文密钥**。

### 5.3 用户侧：自助 Key（全部要求已认证 Portal session）

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| `GET` | `/api/portal/model-groups` | **只返回 `self_service = true 且 enabled = true`** 的组，且只暴露 `name/display_name/description/per_user_max_keys/per_user_max_concurrency/默认配额/是否需审批`，以及**当前用户在该组的已用量**（已建几把、剩几把）。绝不返回内部字段 |
| `GET` | `/api/portal/keys` | 我的 key 列表。**只返回 `plaintext_key_prefix`，绝不返回完整 key** |
| `POST` | `/api/portal/keys` | 创建。body：`{ "name": "...", "group_refs": ["deepseek"], "max_concurrency": 可选(只能调低), "expires_in_days": 可选(不能超过组上限) }` |
| `POST` | `/api/portal/keys/{id}/rotate` | 轮换，返回一次新明文 |
| `POST` | `/api/portal/keys/{id}/toggle` | 停用/启用自己的 key |
| `DELETE` | `/api/portal/keys/{id}` | 删除自己的 key |

**创建时的服务端校验（顺序固定，全部必须实现）**：

1. session 有效、用户 `status = active`；
2. 每个 `group_refs` 存在、`enabled`、`self_service = true`——**否则一律 404，不要 403**，不要让用户能探测到内部组是否存在；
3. 该用户在每个组的现有 key 数 < `per_user_max_keys`；
4. 用户传入的 `max_concurrency` **只能 ≤** 组的 `per_user_max_concurrency`；传大了**直接 400**，不要静默取 min（静默钳位会让用户以为自己拿到了更大的额度）；
5. 物化 `model_allowlist`（**并集非空**，否则 400）与 `model_concurrency_groups`；
6. 生成 key、写 `downstreams`、写 `portal_user_downstreams` 绑定、写审计事件——**必须在同一个事务里**，否则会产生无主 key；
7. **明文只在创建响应里返回一次**，之后任何接口都只给 prefix。

**所有权检查**：`{id}` 类端点必须先确认该 key 的 `owner_portal_user_id == session.user_id`，**不匹配一律 404**（不是 403，不泄露存在性）。管理员创建的、绑给该用户的传统 key（`owner_portal_user_id` 为 `None`）**不允许**被用户删除或改配置，只允许查看和轮换——保持现有 `/api/portal/key/rotate` 的语义。

### 5.4 用户侧：认领存量 Key（§3.5(a)）

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| `POST` | `/api/portal/claim` | body `{ "employee_id": "...", "key": "..." }`。**要求已有有效 OAuth session**；服务端复用 `state.downstream_for_secret()` + `downstream.id == employee_id` 校验（`src/server/portal.rs:52-76`），通过后建立绑定并置 `is_default = true` |

约束：

- 无 session ⇒ `401`，**不要**让它变成一个可以匿名爆破密钥的端点；
- 该 downstream 已被**他人**绑定 ⇒ `409` + 记一条**告警级**审计事件（这是凭据泄漏的信号，不要静默改绑）；
- 已被**本人**绑定 ⇒ 幂等返回 `200`；
- 校验失败的响应必须与"密钥错误"和"工号错误"**无法区分**（沿用现有 `Invalid credentials` 的统一文案）；
- 必须限速（§3.7）。

### 5.5 兼容

保留 `GET /api/portal/key` 与 `POST /api/portal/key/rotate`（`src/server/gateway.rs:2558-2559`）不变，语义为"我的默认 key"（`portal_user_downstreams.is_default`）。**不要**在本期删除它们。

## 6. 开发任务

> 顺序即依赖顺序。每个任务独立可编译、可测试、可提交。
> **S3 必须先于 S7 合并**（容量放大器先上锁再开门）；**S6 必须与 S7 同期上线**（否则存量用户 Day-1 被挡在门外）。

### S1 — 全局模型分组注册表（后端）

- `ModelGroup` 类型 + 校验（组名格式、`patterns` 非空、各上限 ≥ 1、`per_user_max_concurrency ≤ default_max_concurrency`）；
- Postgres migration（新版本号，幂等）；单开 `ModelGroupStore` trait，`AppState` 持有；
- 管理员分组 CRUD（§5.1 前 5 项），`DELETE` 被引用时拒绝；
- **复用 `glob_match_star`（`src/state/types.rs:1161`），不要另写匹配器**——两套匹配语义早晚分叉。

### S2 — 物化与重新物化

- `materialize_groups(refs, overrides) -> (model_allowlist, model_concurrency_groups, max_concurrency)` **纯函数、可单测**，不要写进 handler；
- `POST /{name}/rematerialize`，逐 key 报告；
- `stale` 判定（`model_groups_materialized_at < group.updated_at`）；
- `DownstreamConfig` 新增三字段（§4.2），确认旧 JSON（无这些字段）仍能反序列化。

### S3 — 并发按人聚合（§3.3，**安全关键，先于 S7**）

- C7 计数键 `(downstream.id, group)` → `(scope, group)`，`scope = owner_portal_user_id.unwrap_or(downstream.id)`；
- **本地后端与 Redis 后端都要改**（C7 两边都实现了，见 commit `a47f2373`）；
- 全局兜底 `max_concurrency` 同样按 scope 聚合；
- 支持 §5.2 的 per-user `quota-overrides`：scope 预算取 `override.unwrap_or(group.per_user_max_concurrency)`。

### S4 — 容量账本（§3.6）

- 纯函数：给定组的 `patterns` + 上游快照，算出上游真实容量（遍历 `supported_models`（`src/state/types.rs:740`）与 `api_key_models[].supported_models`（`:864-869`），命中的 key 数 × 该上游 `max_concurrency`）；
- 下游已分配：按 scope 聚合求和；
- `GET /api/admin/model-groups/{name}/capacity`；
- **超卖不阻断，只警示**，但口径说明要写进响应与 UI。

### S5 — 管理员 Portal 用户管理（后端，§5.2）

- 用户列表/详情/启停/绑定/解绑/批量预绑定/配额覆盖/吊销 session；
- **停用用户 ⇒ 同步停用名下自助 key + 吊销全部 session；启用不自动恢复 key**；
- 审计事件表 + `GET /api/admin/portal-audit`。

### S6 — 存量用户认领（§3.5，**与 S7 同期上线**）

- `POST /api/portal/claim`，复用 `state.downstream_for_secret()` + id 比对（`src/server/portal.rs:52-76`）；
- 无 session ⇒ 401；他人已绑 ⇒ 409 + 告警审计；本人已绑 ⇒ 幂等 200；
- 限速（§3.7）；
- 前端：首次登录无绑定时跳认领页，而不是甩一个 403 错误页。

### S7 — 用户自助 Key（后端，§5.3）

- 6 个端点 + 全部校验（顺序固定，见 §5.3）；
- 建 key + 写绑定**同事务**；
- 审计事件（不含明文）；
- 全部挂在 `PORTAL_SELF_SERVICE_KEYS_ENABLED`（默认 **false**）之下。

### S8 — 滥用防护与生命周期（§3.7）

- 创建/轮换/认领端点的 per-user 与 per-IP 限速；
- 自助 key 默认过期（组的 `default_expires_in_days` → `DownstreamConfig.expires_at`，字段已存在）；
- 到期前 Portal 提示；到期后失效。

### S9 — 前端

- **管理员**：模型分组管理页（列表含 `key_count` / `stale_key_count` / 容量账本，增删改，重新物化带影响面确认弹窗列出将被改动的 key）；Portal 用户管理页（列表/详情/启停/绑定/配额覆盖/吊销 session/审计查询）；下游列表展示归属人与组、可按归属人筛选。
- **用户**：认领页；"我的 Key"页（列表只显示 prefix、创建向导「选组 → 看剩余额度 → 命名 → 确认」、轮换/停用/删除）；创建成功后**一次性**展示明文并明确提示"关闭后不再显示"；组选择器展示 `display_name` + `description` + 剩余可建数量。

### S10 — 审批流（可选，`requires_approval`）

- `portal_key_requests` 表 + 用户提交 / 管理员审批端点 + 两端 UI；
- **一期不做就把字段保留但拒绝设为 true**，不要留一个设了没用的开关。

### S11 — 运维文档

- 分组配置指引（承接 `2026-08-27-upstream-concurrency-lease-trustworthy-accounting.md` §C7）；
- `per_user_max_keys` / `per_user_max_concurrency` 取值建议与容量账本口径；
- 灰度上线步骤（§8）、认领流程话术、离职回收流程。

## 7. 测试要求

**基线**：以实施时 `rtk proxy cargo test` 的实际数字为准（2026-08-28 最近一次记录为 **1825 passed**）。开工前先自己跑一次确认，**不要照抄**，这棵树在动。

**验证纪律**：`rtk proxy cargo test 2>&1 | tail -40` + `echo "TRUE_RC=${PIPESTATUS[0]}"`；统计套件数要重定向到文件再统计，别从 `tail` 结果里数；验证步骤**不用 `&&` 串联**；不要 `git add .`。

### 7.1 S1/S2 分组与物化

- `materialize_groups` 单测：单组、多组并集、模式重叠时顺序即优先级、override 调低生效、调高被拒；
- **并集为空时创建被拒**（钉死 `src/state/usage.rs:248-250` 空 allowlist = 放行全部的坑）；
- 旧 `DownstreamConfig` JSON（无新字段）能反序列化，行为逐字节不变；
- 组被引用时 `DELETE` 返回 409 且列出引用者；`rematerialize` 后 `stale_key_count` 归零。

### 7.2 S3 并发聚合（**上线门槛**）

- **一个用户建 3 把 key、每把组上限 28，三把并发合计仍被压到 28**——缺了这条整个功能就是不安全的；
- 管理员创建的传统 key（`owner_portal_user_id = None`）行为与改动前逐字节一致；
- 同组不同用户预算互相独立；
- per-user `quota-overrides` 生效且只影响该用户；
- Redis 后端与本地后端结论一致。

### 7.3 S4 容量账本

- 上游容量计算命中 `supported_models` 与 `api_key_models[].supported_models` 两处；
- 超卖时返回警示但**不阻断**创建。

### 7.4 S5 管理员用户管理

- 停用用户 ⇒ 名下自助 key 全部停用 + session 全部失效；**重新启用不自动恢复 key**；
- 解绑**不删除** key；
- 批量预绑定逐条成功/失败；
- 所有写操作产生审计事件，且事件体内**不含明文密钥**（要有断言）。

### 7.5 S6 认领

- 无 session ⇒ 401；
- 工号或密钥错误 ⇒ 401，且两种错误**响应不可区分**；
- 他人已绑 ⇒ 409 + 告警审计事件；本人已绑 ⇒ 幂等 200；
- 限速生效。

### 7.6 S7 自助创建

- 选中 `self_service = false` / `enabled = false` 的组 ⇒ **404**（不是 403）；
- 超过 `per_user_max_keys` ⇒ 400 且说明剩余额度；
- `max_concurrency` 传大于组上限 ⇒ **400，不是静默钳位**；
- 明文只在创建响应出现一次，`GET /api/portal/keys` 只有 prefix（断言响应体不含完整 key）；
- 操作**别人**的 key ⇒ **404**；操作管理员创建的传统 key 的 delete/改配置 ⇒ 拒绝，rotate 仍可用；
- 用户被 disable / session 被 revoke 后所有自助端点立即拒绝（**每请求重新检查**）；
- 写 `downstreams` 成功但写绑定失败 ⇒ 事务回滚，**不留无主 key**；
- 无 Postgres 的文件模式下自助端点返回 `503`。

### 7.7 业务连续性（§3.4，**必测**）

- **关掉 `PORTAL_SELF_SERVICE_KEYS_ENABLED` 并断开 portal 存储后，一把自助创建出来的 key 仍能完成一次推理请求**——这条证明数据面零耦合；
- 存量 key（无新字段）在全部改动落地后行为不变；
- 旧 `POST /api/portal/login`（工号+密钥）与 `/api/portal/key`、`/key/rotate` 继续可用；
- 管理员登录不回归。

### 7.8 前端

- 组选择器只显示自助组，剩余额度正确；
- 明文一次性展示，刷新后不再出现；
- 重新物化的影响面弹窗列出正确的 key 列表；
- 首次登录无绑定时进入**认领页**而不是错误页。

## 8. 业务连续性与灰度上线

**分五步走，每一步都能独立停下且不影响存量业务。**

| 步骤 | 内容 | 对存量的影响 | 回退 |
| --- | --- | --- | --- |
| 1 | 部署 schema（`model_groups`、`portal_*`、审计表）+ 未启用的代码，`PORTAL_OAUTH_ENABLED=false`、`PORTAL_SELF_SERVICE_KEYS_ENABLED=false` | 无 | 代码回滚，表保留（additive） |
| 2 | 管理员配好模型分组（S1/S2/S4/S9 管理端），先**不**开 `self_service` | 无 | 删组即可 |
| 3 | 对少数试点用户开 OAuth（S6 认领链路同时可用） | 试点用户可用两种登录方式，其余用户不变 | 关 `PORTAL_OAUTH_ENABLED`；已建 session 按 TTL 或显式吊销 |
| 4 | 开 `PORTAL_SELF_SERVICE_KEYS_ENABLED`，先只对 1–2 个组开 `self_service` | 存量 key 不变 | 把组的 `self_service` 改回 false，已建 key 继续可用 |
| 5 | 全量开放；旧「工号+密钥」登录进入**兼容期**（建议 ≥ 1 个季度）并公告弃用时间 | 兼容期内两套登录并存 | 兼容期内随时可停止推进 |

**必须守住的连续性约束**：

1. **旧登录不在本期删除。** `POST /api/portal/login` 与 `/api/portal/key`、`/key/rotate` 保持可用，弃用要单独走公告 + 兼容期。
2. **存量 key 不迁移、不改写。** 它们 `model_group_refs` 为空、`owner_portal_user_id` 为 `None`，走原有语义。想把某把老 key 纳入分组管理，是管理员的**显式**操作，不是自动批处理。
3. **回滚不断服务。** 见 §3.4：控制面回滚后，已建的自助 key 仍是普通 downstream，继续可用。
4. **Day-1 不能把存量用户挡在门外。** S6 认领必须与 S7 同期上线；只有 S7 没有 S6 = 所有老用户看到 403。
5. **先上锁再开门。** S3（人维度聚合）必须先于 S7（自助创建）合并。

## 9. 风险与回滚

| 风险 | 说明 | 处置 |
| --- | --- | --- |
| **容量放大（最高风险）** | 自助建 key 无人维度聚合 ⇒ 用户可无限放大并发 | **S3 必须先于 S7 合并**；S3.4 三 key 聚合测试是上线门槛 |
| **Day-1 存量用户被挡在门外** | 只有"无绑定 ⇒ 403"这一条规则，老用户全部进不来 | **S6 认领必须与 S7 同期上线**（§3.5）；灰度按 §8 走 |
| **控制面故障波及推理** | Postgres / OAuth / Portal 挂掉 | §3.4 数据面零耦合；§7.7 断存储后自助 key 仍可推理的回归测试 |
| **空 allowlist 放行全部模型** | 物化漏写就等于全模型权限（`src/state/usage.rs:248-250`） | 物化并集为空一律拒绝创建 + 专门回归测试 |
| **认领端点变成爆破面** | `/api/portal/claim` 收工号 + 密钥 | 必须已有有效 session 才能调用 + 限速 + 两类错误响应不可区分 |
| **改绑被静默覆盖** | 他人认领同一把 key | 一律 409 + **告警级**审计事件，绝不静默改绑 |
| **合法用户数量堆出的超卖** | §3.3 只挡单人，挡不住 100 人各拿 4 并发 | §3.6 容量账本让超卖**可见**；超卖不阻断，但必须能被看见 |
| **离职回收不确定** | 停用了用户但 key 还活着 | 停用用户 ⇒ 同步停用其自助 key + 吊销全部 session；启用**不自动**恢复 key |
| **无主 key** | 建 key 与写绑定不在同一事务 | 事务覆盖；另加后台巡检，报告 `owner_portal_user_id` 指向不存在用户的 key |
| **改组后静默不一致** | key 上是物化副本，改组不自动生效 | `stale_key_count` 在 UI 可见 + 显式 `rematerialize`；**不要**改成自动生效 |
| **明文泄漏** | 列表接口误返回完整 key | 列表只出 prefix；加断言：响应体不含完整 key |
| **枚举内部分组 / 他人资源** | 用 403/404 区分能探测存在性 | 一律 404 |
| **自助 key 长期堆积** | 无人认领的 key 越积越多 | `default_expires_in_days` 建议非空 + 管理员按归属人筛选/批量停用 |

**回滚粒度**：

- **S1/S2/S4/S5（管理员侧）** 可独立回滚，不影响任何现有 key；
- **S6/S7/S8/S9 用户侧** 由 `PORTAL_SELF_SERVICE_KEYS_ENABLED`（默认 **false**）控制，关掉即回到现状；
- **已创建的自助 key 在任何回滚场景下都继续可用**——它们就是普通 downstream（§3.4）；
- **S3 是纯正确性增强**，`owner_portal_user_id` 为 `None` 时行为逐字节不变，**不加开关**；
- 数据库表全部 additive，代码回滚时**保留**表，避免破坏已建立的绑定关系。

## 10. 任务回填表

> 逐行回填 commit hash 与结果，通过打 ✅，未做写明原因。**不要提前打 ✅。**

| 任务 | 内容 | commit | 结果 |
| --- | --- | --- | --- |
| S1.1 | `ModelGroup` 类型 + 校验 + migration | | |
| S1.2 | 管理员分组 CRUD + 被引用拒绝删除 | | |
| S2.1 | `materialize_groups` 纯函数 + 单测 | | |
| S2.2 | `rematerialize` + `stale` 判定 | | |
| S2.3 | `DownstreamConfig` 三新字段 + 向后兼容 | | |
| S3.1 | 并发计数键 `(scope, group)`（本地后端） | | |
| S3.2 | 同上（Redis 后端） | | |
| S3.3 | per-user `quota-overrides` 接入 scope 预算 | | |
| S3.4 | **三 key 聚合回归测试（上线门槛）** | | |
| S4.1 | 上游容量计算纯函数 + 单测 | | |
| S4.2 | `/model-groups/{name}/capacity` | | |
| S5.1 | 用户列表/详情/启停/绑定/解绑/批量预绑定 | | |
| S5.2 | per-user 配额覆盖 + 吊销 session | | |
| S5.3 | 审计事件表 + `/api/admin/portal-audit` | | |
| S6.1 | `POST /api/portal/claim` + 全部约束 | | |
| S6.2 | 首次登录无绑定跳认领页（前端） | | |
| S7.1 | 自助 6 端点 + 全部校验 | | |
| S7.2 | 建 key 与绑定同事务 + 审计 | | |
| S7.3 | `PORTAL_SELF_SERVICE_KEYS_ENABLED` 开关 | | |
| S8.1 | 创建/轮换/认领限速 | | |
| S8.2 | 自助 key 默认过期 + 到期提示 | | |
| S9.1 | 管理员：模型分组页（含容量账本 + 影响面弹窗） | | |
| S9.2 | 管理员：Portal 用户管理页 + 审计查询 | | |
| S9.3 | 用户：认领页 | | |
| S9.4 | 用户：我的 Key 页 + 创建向导 + 明文一次性展示 | | |
| S10 | 审批流（或明确不做） | | |
| S11 | 运维文档 + 灰度步骤 | | |
| — | **§7.7 业务连续性回归（断存储后自助 key 仍可推理）** | | |

### 10.1 验证结果回填

| 步骤 | 命令 | 退出码 | 结果 |
| --- | --- | --- | --- |
| fmt | `rtk proxy cargo fmt --check` | | |
| clippy | `rtk proxy cargo clippy --all-targets` | | |
| test | `rtk proxy cargo test` | | 套件数 / passed / failed / ignored |
| 前端类型 | `cd frontend && npm run type-check` | | |
| 前端测试 | `cd frontend && npm test` | | |
| Postgres | 分组/用户/自助集成测试 | | 通过数 或「未执行（无 Postgres）」 |
