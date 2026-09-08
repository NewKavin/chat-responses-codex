# 门户用户配置合并、模型权限统一与总览局部刷新：设计与实施方案

- 日期：2026-09-07
- 状态：设计修订稿，待开发；本文中的新表、接口、行为和测试尚未实现。
- 目标：门户用户管理完整承接下游配置和批量操作；用户默认可使用其所有授权分组的模型；模型候选、门户、客户端目录与请求鉴权一致；总览后台刷新不打断页面。
- 技术栈：Rust / Axum / PostgreSQL，Vue 3 / Element Plus / ECharts / Vitest。
- 配套执行说明：[开发提示词](../prompts/2026-09-07-model-group-single-source-prompt.md)。开发按本文逐项执行，子代理仅用于探索和独立核验。
- 历史依据：[白名单退休方案](2026-09-06-retire-model-allowlist.md)、[下游页面合并记录](2026-09-07-portal-users-consolidate-downstream.md)。历史文档的“已完成”不替代本方案的行为验收。

## 0. 范围与取舍

本次合并管理入口与模型授权事实源。原有按密钥统计、限流、并发、请求配额及计费的归属保持原义，不自动改为用户共享额度。

“用户所有分组”指该用户明确拥有的分组，加当前产品保留的默认 `basic`。不包含其他用户通过共享密钥关联的分组。新门户密钥默认继承此并集；旧密钥显式限制仍可保留。

| 方案 | 结论与理由 |
|---|---|
| 同步写两处旧 model_group_id | 不采用。仍有两个事实源，批量更新、撤权和缓存会继续分歧。 |
| 将 deny-all 解释为继承 | 不采用。删组的外键兜底会变成放权，显式禁止也无法表达。 |
| 用户授权 + 独立密钥访问策略 | 采用。区分归属与模式，旧配置作为迁移输入，同一解析结果供展示与鉴权使用。 |

独立策略表适配现有门户授权独立写库的结构，也避免不相关的 downstream 全量配置快照用旧值覆盖新权限。原 downstream 配置继续承载额度与计费，访问策略有唯一写入服务。

## 1. 已核实的根因

以下位置以修订时的代码为依据，开发时以符号定位复核。

| 问题 | 代码证据 | 结果 |
|---|---|---|
| 三层权限脱节 | 用户授权为 portal_user_model_groups，绑定组为 portal_user_downstreams.model_group_id，密钥组为 downstreams.model_group_id。`state.rs:6245 effective_model_allowlist` 只读第三层，`gateway.rs:5683 get_key_allowed_models` 又读第二层。 | 选择绑定组后密钥仍为 deny-all，目录为空、概览出现 __none__、请求 403。 |
| 批量改组被拒绝 | `PortalUsers.vue:805 batchApplyGroup` 调 /api/admin/downstreams/batch-update，但 `admin.rs:2721 BATCH_UPDATE_DOWNSTREAM_ALLOWED_FIELDS` 不含 model_group_id。 | 返回 400，没有保存。 |
| 候选方向错误 | `ModelGroupForm.vue:83 loadModelCandidates` 将 aliases 当目标、canonical 当原名，又合并原始 supported_models。 | 原名回流、展示名被去掉，跨上游映射被混成全局规则。 |
| 列表与请求身份不同 | `state.rs:6363` 按原名过滤后改展示名；`gateway.rs:5641` 对客户端入参鉴权；`usage.rs:275 model_list_allows` 不解析别名。 | 改名后可见却不可调用，或有权限却不可见。 |
| 操练场重复过滤 | `Playground.vue:437 loadModels` 同时读 quota 和 /v1/models；`utils/playground.ts:186 selectPlayableModels` 再取交集。 | ["*"] 被当作字面模型过滤；默认绑定与使用中的密钥还可能不同。 |
| 刷新跳动 | `Dashboard.vue:1131/1205` 轮询时置 loading，:203 空态条件包含 loading，:1255 每 5 秒执行。 | 遮罩闪烁，空态与表格切换改变高度，行状态也难以保留。 |
| 配置迁移未闭环 | `PortalUsers.vue:824/839` 批量重置默认值并全部提交；saveEditConfig 后只刷新绑定；`admin.rs:2273 apply_downstream_updates` 不处理 expires_at。 | 未改字段被覆盖、回显缓存过时、过期时间控件不生效。 |

补充事实：get_key_allowed_models 使用无聚合的 query_opt，多用户绑定时会因多行报错，并非随机选择用户。删组外键最终落 deny-all（`postgres.rs:1196 migrate_downstream_group_fallback`）；解绑只删除绑定，不撤销 secret（`portal_store.rs:640 remove_downstream_binding`）。

## 2. 管理界面与配置承接

### 2.1 单一入口

- 继续移除“下游管理”导航；旧 /admin/downstreams 重定向门户用户管理，保留有意义的用户或密钥筛选参数。
- 用户列表保留身份、状态、授权分组、密钥数，补充有效模型数。入口改为“配置”，以一个响应式抽屉承载“用户与授权”“密钥配置”“用量”，不层层叠加绑定及编辑弹窗。
- 用户列表支持多选，批量操作区分“用户授权”和“用户下的密钥配置”；提交前列出实际用户数、去重后的密钥数及目标 ID。
- 同一页面提供“全部密钥”“未关联用户”“待处理迁移记录”筛选，支持跨用户批量和存量直连密钥配置，不恢复下游页签。
- 密钥列表保留名称/ID/用户/启用/过期筛选、分页、列配置、运行并发、额度消耗和日志入口。
- 管理员界面不创建、不展示复制 secret、不轮换密钥；用户在门户自建。管理员保留配置、授权、启停和存量归属处理。既有 key- 登录凭据不因页面合并而删除。

### 2.2 原配置承接清单

| 配置/信息 | 新位置 | 批量及数据语义 |
|---|---|---|
| 名称、ID、启用状态 | 密钥配置 | ID 只读，名称单条修改，启停可批量。 |
| 用户授权分组 | 用户与授权 | 多用户批量增加/移除/替换；basic 保留不可撤销规则。 |
| 密钥模型权限 | 密钥配置 | 继承、限定分组、拒绝全部；可跨用户批量，逐密钥计算效果。 |
| 限额开关、每分钟请求数 | 密钥配置 | 可批量；关开关保留原数值，运行时沿用既有跳过规则。 |
| 配额窗口小时、窗口请求次数 | 密钥配置 | 可批量，保留既有上下界及拦截语义。 |
| 全局最大并发、模型并发组 | 密钥配置 | 可批量替换/明确清空并发组；保留全局兜底及匹配规则。 |
| 每日/月 Token 字段 | 密钥配置 | 可批量设置/清空，保留原参考或拦截含义。 |
| 计费模式、输入/输出单价、每日金额上限 | 密钥配置 | 可批量，金额按整数分传输，不改变计费算法。 |
| IP/CIDR 白名单 | 密钥配置 | 单条/批量设置或清空；空数组仍表示无限制。 |
| 过期时间 | 密钥配置 | 单条/批量设置或清空；API Unix 秒，控件毫秒仅在边界转换。 |
| 标签、默认请求密钥、归属 | 密钥列表 | 默认标记只在明确提交时改变，标签与配置名称分别展示。 |
| 运行中/等待/占用/上限、用量和日志 | 密钥列表/用量 | 只读局部刷新，失败标记不可用，不补假零。 |

### 2.3 批量与保存

- 每个字段默认“保持不变”，勾选后才进入 PATCH。缺失=不修改，合法值=设置，明确 null=清空可空字段；不可空字段拒绝 null，空 PATCH 返回 400。
- 单条/批量共用字段校验、策略写入和单位转换，补全 expires_at、限额开关、IP、并发组等。
- 沿用 `{ updated: string[], failed: [{ id, error, code? }] }`；部分失败不能显示全部成功。成功项回读配置，失败项保留选中并可重试。
- 保存前锁定目标 ID；切换用户/关闭抽屉清空选择。成功后按 ID 合并服务端配置与权限结果，刷新摘要，不能只刷新 bindings。
- 用户资料和授权分别保存为明确操作。授权集合替换在一个事务内完成，不再逐条 grant/revoke 后可能半套生效。

## 3. 权限模型

### 3.1 模式与持久归属

新增 PostgreSQL 表 `downstream_access_policies`，作为数据库模式下每把密钥唯一访问策略：

| 字段 | 约束与含义 |
|---|---|
| downstream_id | 主键，引用 downstreams(id) ON DELETE CASCADE。 |
| subject_kind | direct 或 portal；解绑不能将 portal 自动改回 direct。 |
| owner_user_id | 可空，引用 portal_users(id) ON DELETE SET NULL；portal 空归属表示无权限。 |
| mode | inherit/group/deny；新门户密钥默认 inherit，新直连默认 deny，兼容创建接口显式带组时为 group。 |
| model_group_id | NOT NULL DEFAULT 'deny-all'，引用 model_groups(id) ON DELETE SET DEFAULT；仅 group 模式读取。 |
| revision | 单行递增版本，用于策略冲突检测与回读，不与旧字段双向同步。 |

direct 不允许 inherit 或 owner；portal 的空 owner 允许持久化，用于解绑/删除用户后的封闭状态。inherit/deny 模式将 model_group_id 写为 deny-all 占位，**mode 决定是否继承，绝不从组名推断继承**。

旧 downstreams.model_group_id、portal_user_downstreams.model_group_id 保留升级前数据，迁移后不作为数据库模式的权限读源。API 同名字段从新策略派生。此版本不删旧列、白名单表或登录凭据。

### 3.2 计算规则

定义 U(user)=basic 与用户显式授权分组的模型身份并集；G(key)=密钥显式限定组。集合类型采用 `All | None | Models(Set<ModelId>)`，不让空数组同时表示放行和拒绝。

| 状态 | 有效权限 |
|---|---|
| portal，唯一有效 owner，inherit | U(owner) |
| portal，唯一有效 owner，group | U(owner) ∩ G(key) |
| portal，deny | None |
| portal，owner 缺失/禁用/归属冲突 | None，附明确原因，不进入 direct 分支 |
| direct，group | G(key) |
| direct，deny | None |

All ∩ X = X，None ∪ X = X，None ∩ X = None。空业务组不合法，deny-all 始终为 None，all 始终为 All；新核心逻辑不传递 __none__。

active=false、过期、IP 和原配额检查继续独立生效。授权不保证上游瞬时健康或额度充足。门户用户可在用户上限内设置自己密钥的可选限定组/恢复继承，但不能解除管理员的 active=false 或 deny；解除 deny 由管理员执行。

### 3.3 归属与生命周期

- 新门户密钥只有一个 owner，配置、策略及绑定持久化整体成功后才返回 secret。并发绑定锁定 downstream 行，第二位用户认领返回 409 key_owner_conflict。
- 旧多用户绑定标为 ownership_conflict 并拒绝 API，保留原记录供核对。确认归属用于整理历史，被多人持有的旧 secret 不自动恢复 API，用户自行创建替代密钥。
- 解绑保留 portal 主体，owner 置空、mode=deny。原 L3=all 也不能因为绑定数量归零而获得全权限。
- 禁用用户同时注销会话并拒绝新目录/模型请求，恢复启用按现有策略重新计算。用户删除导致空 owner 同样拒绝；不能过滤 disabled 用户后以“无归属”回退直连模式。
- 门户删除密钥必须在成功响应前撤销 API 可用性；历史日志所需记录可保留。解绑不等于 secret 撤销。
- 轮换复制 owner、模式、限定组、限额、计费、IP、过期等全部配置，并撤销旧 secret；不重新落 all 或默认额度。
- 存量 direct 绑定到用户是显式转换：同一事务设置 portal/owner、保留原限制，显示权限预览；转换后不自动回 direct。
- 管理端旧 create/rotate API 本次保留兼容，但必须接入新策略与生命周期约束，UI 无入口，不得绕过 owner、禁用或撤销。

首次登录也是归属写路径：legacy 工号 JWT 的 sub 仍是 downstream ID，通过新策略 owner 定位用户；首次从 direct 登录建档时，将原权限作为该用户的既有授权和密钥限定整体迁入。`ensure_user_for_downstream` 的新建和已有绑定两个分支都要接入，不能继续按首条绑定挑 owner。

OIDC 当前只建用户、不自动发 key，且 callback 在无默认密钥时返回 portal_access_not_granted。新行为是：合法、启用且符合既有注册策略的用户可建立门户会话，无密钥状态进入密钥自助创建页；不自动生成凭据。原 OIDC bind intent 也必须走同一 owner 校验，不能只检查“用户是否绑定过其它 key”。

### 3.4 解析服务与失败语义

新增 `src/state/model_access.rs` 承载集合组合、策略解析与批量读取，AppState 提供调用入口：

```rust
enum AllowedModels { All, None, Models(BTreeSet<ModelId>) }
enum AccessMode { Inherit, Group, Deny }
// ModelId 由 model_catalog 构造，上游 wire 名不能直接当作它。
// ResolvedModelAccess 包含 allowed、owner、来源组、mode、拒绝原因。
// 查询失败通过 Result 返回，不转换为 All 或旧白名单。
```

- 单条/批量共用组合函数。数据库模式读取新策略、owner 状态、授权和组内容，不从旧 DownstreamConfig 字段/默认绑定推断。
- 批量在一个 REPEATABLE READ READ ONLY 事务中加载：策略及 owner 一次、用户授权一次、相关组一次；循环内不逐密钥/模型查询。
- 缺策略或查询失败返回错误；删除组按 deny-all 拒绝；坏 JSON 不得 unwrap_or_default 成放行。effective_model_allowlist/_map/_opt 作为适配器，数据库错误不能回退白名单。
- 查询错误时目录与推理返回 model_group_check_failed；确定无权限时空目录/403。展示可保留上次成功快照并标记不可用，不能把失败当无限制。
- 事务提交后新请求立即使用新权限，本版不加权限 TTL 缓存。每次请求内复用同一权限快照，避免多道检查读取不同版本。
- 路由配置快照持久化成功后再发布，失败不提前更新内存。权限表不由旧配置快照整体保存覆盖。
- 文件模式无 PortalStore，现有行为实际回退 legacy model_allowlist，并非只看 L3。本版保留该兼容适配（旧空白名单=All），不提供依赖数据库的用户授权。若加载了 portal 策略但没有 store，必须拒绝，不能回退放行。

必须审计 `insert_downstream`、导入/replace/sync 及新 ID 创建路径。现有 sync_downstreams 对保留 ID 是 UPSERT，仅删除快照中缺失的 ID；新表不会因 UPSERT 自动初始化。所有新增 ID 必须同事务创建策略，删除 ID 会级联删除策略，不能通过删后重建把原限制变成默认值。内存测试辅助 add/delete 不能冒充 PostgreSQL 持久化路径。

资格审定 `apply_model_qualification` 也要迁移：目标限定组从新策略取得，仅 mode=group、非内置、仅被该 key 策略引用、且没有任何用户授权引用的组可被原地修改。其它情况返回明确 conflict，要求先选择专属限定组；不得用旧 downstreams.model_group_id 的引用数判断安全性。组内容更新与目标配置仍同事务，不能因审定一个 key 改变其它 key 的权限上限。

## 4. 存量迁移与当前故障处理

### 4.1 启动事务迁移

接入 `PgStore::initialize_schema`，位于现有白名单迁组、deny-all 外键及门户标记迁移之后，接流量之前完成。`migrations/*.sql` 仅留档，不假定有自动执行器。

同事务建策略表、读旧快照、分类、写审计和迁移版本。新增 `downstream_access_migrations`，以 downstream_id + migration_version 唯一标记，记录旧 L1/L2/L3、原有效集合、迁移后策略、分类、待处理原因及解决时间；不记录 secret/hash。

Eold(key) 按旧请求闸门顺序计算 L3 与 L2 的交集，处理旧 wildcard、大小写和受控后缀语义。使用持久化配置，不因上游暂时离线/冷却/缺探测结果删除权限。别名归一导致的等价名称变化单列，不冒充逐字符串无差异。

| 存量类别 | 自动处理 | 保证 |
|---|---|---|
| 无门户归属证据的 direct | 由旧 L3 建 group/deny；文件白名单迁组沿用已有逻辑 | 原允许/拒绝模型不扩大 |
| 单 owner，Eold 非空 | portal/owner；保留 Eold 为限定组，补足该 owner 对这部分既有权限的授权 | 旧密钥保留原模型；新继承密钥使用用户合法权限并集 |
| L3=all、L2=premium，L1 无 premium | 现有生效 L2 权限补为用户授权，密钥保留 premium 限定 | 不丢高级模型，也不因 basic 增加旧密钥权限 |
| 两层有限定，交集不同于现有组 | 按 Eold 生成去重的 migrated-access-<hash> 组，作为限定组并授给 owner | 不将完整 L2 越过旧 L3 授予；生成组是迁移时快照，来源可追溯 |
| L3=deny-all 且绑定业务组，或空交集 | 保留拒绝，标 review_required，记录现有授权和旧绑定组作为修复候选 | 不猜测默认误留还是主动禁止，不自动恢复流量 |
| 多用户绑定 | portal、空 owner、deny，标 ownership_conflict | 不相加多个用户权限 |
| 已识别为门户密钥但无 owner | portal、空 owner、deny，标 orphan | 不从旧 all 获得全权限 |
| owner 已禁用 | 保留 owner 和可恢复的限定策略，运行时拒绝 | 升级不解除禁用 |

补授权只限旧生效权限；既有显式授权仍保留。迁移后创建/保存绑定不再反向补授权。旧权限包含 All 时，审计明确标记授权范围。无法解析的旧组标记需处理并拒绝，不伪造成 All。

不复制额度、不清空 IP、不改变 active/过期、不轮换 secret、不删日志。幂等重跑不能覆盖管理员之后的修改；数据库错误整体回滚，不能边迁移边接流量。

### 4.2 修复“选了分组仍为 __none__”

门户用户详情显示待确认记录及原因，管理员多选后执行“应用用户分组”：

1. 预览每位 owner 的现有授权、待补旧绑定组、每把密钥变更前后的模型与模式。
2. 仅提交选中的归属明确记录；如选择补组，同时更新用户授权，选中密钥设为显式 inherit。归属冲突不能自动混入。
3. 每位用户的授权及选中策略在一个事务提交；不同用户可分别成功/失败，返回逐用户/密钥结果。
4. 保存后用户模型列表、继承密钥目录和真实请求立即同口径生效，无需手工 SQL、重启或重建 secret。

该操作是产品功能，不是迁移默认开启的“全部修复”。普通用户编辑分组仅修改用户授权，inherit 密钥自动生效；旧显式限制/禁止状态保持可见，不被静默覆盖。

### 4.3 升级与回滚

- 隔离库使用真实旧 schema 形态验迁移；升级前保存数据库备份及按 key 的目录/权限样本，升级后对照审计报告。
- 允许差异只限标明的身份等价修正、原异常封闭和明确选择的修复，不能笼统声称无差异。
- 旧字段留存不代表只换旧二进制即可回滚。新授权/继承/归属变更旧程序无法读取；规定回滚路径为停流量并恢复配套升级前数据库备份，不承诺无数据损失的代码单独回退。
- 同库禁止新旧权限语义网关混跑，发布一次切换全部实例；Redis 不能解决权限语义混跑。

## 5. 模型身份与目录一致性

### 5.1 对外身份生成

新增 `src/state/model_catalog.rs`，基于 effective_downstream_models_detailed、alias registry、model_identity 生成 PublishedModel，包含对外 ModelId、显示名及上游 wire 路由。业务接口不返回上游 secret。

1. 读取活跃上游的持久化可路由模型及有效 key 映射；已发现的权威空集合保持为空。
2. 每个上游分别应用 model_mappings：有映射用 downstream_model 原样；无映射才用原名，随后按全局 alias 的 canonical 生成展示身份。
3. canonical 是对外名，aliases 是归一拼写；显式 per-upstream 映射标签优先，不被另一个全局 alias 再改名。
4. 统一身份后去重并稳定排序。某上游原名被映射，不删除另一未映射上游发布的同名模型；失去持久路由的映射不进入可用目录。
5. 健康状态独立展示，429/冷却不使目录模型消失。组里已有但暂时无路由的模型保留并标记，不能静默删配置。

目录过滤、组集合组合、请求鉴权共用 resolve_public_model_id 和 allows_public_model。权限保留既有大小写不敏感及受控 subagent 后缀语义；路由比较/目录去重继续尊重现有运行时大小写开关，测试两种值。

全局 `deepseek-chat -> deepseek-v3` 是一个身份：组使用 alias 或 canonical，目录发布 canonical，提交 canonical 均通过同一权限检查。不得仅在列表用 `allows(display) || allows(raw)`。

上游 wire 原名不会因是映射源就自动成为客户端有效名；仅当它独立发布或被明确的全局 alias 规则接受时才可解析，否则返回既有模型不存在/不可路由错误。

### 5.2 完整接入范围

| 消费方 | 统一规则 |
|---|---|
| 普通 /v1/models | 权限 ∩ PublishedModel，返回对外名称 |
| /v1/models?format=codex | 同一集合再加能力/上下文元数据，不追加未匹配路由的白名单字符串 |
| chat/completions、responses、messages、count_tokens | 同一 ModelId 与权限谓词；能力失败仍使用原错误体系 |
| 门户概览、配额、操练场、集成页 | 明确用户/选定密钥作用域，不拿默认密钥 quota 过滤另一密钥 |
| 模型统计与上下文清单 | 结果键为对外 ID，wire 名仅用于对应路由的上下文查询 |
| 门户模型探测 | 授权对外模型定位路由，wire 名发探测，结果映射回对外 ID |
| usage summary、可见模型统计、能力探测排队 | 共用权限快照与身份；历史 usage 不按新分组删除/重写 |
| 管理端 scope=exposed | 无用户过滤的 PublishedModel，供组表单选择 |

前端以服务端模型列表为准，移除 selectPlayableModels 基于 quota 的二次交集；不把 * 当模型，不因请求失败回退无限制。

### 5.3 门户作用域与空态

- 新增 `GET /api/portal/model-access`：无 downstream_id 返回用户授权并集的可路由模型；带 ID 则先校验 owner，再返回密钥有效模型。原 /api/portal/models 统计结构不变。
- 响应含 scope、downstream_id、available_models、source（user_group_ids/mode/key_group_id）、status 和 reason。status=ready/denied/no_routes；查询失败用非 2xx，不伪造正常空结果。
- 新契约 available_models=[] 只表示无模型。旧 quota.model_allowlist 暂作兼容投影：All=["*"]，None=["__none__"]；新界面不再直接消费它。
- 概览显示“可用模型”，默认用户范围与授权来源。无密钥也能看授权模型，并可在门户自建；额度区仍明确属于所选密钥。
- 操练场可见用户全部模型，按当前请求密钥标明可调用/受限。首次可优先使用现有 inherit 密钥，否则保留默认密钥；不后台创建密钥、不因额度不足偷偷换密钥。受限模型保留可见并说明原因，用户自行选择合适密钥。
- 手工外部 Bearer 调试以该凭据 /v1/models 为准，不混入登录用户 quota。切换用户/密钥取消旧请求、清理旧选择；模型撤权阻止提交，服务端仍最终校验。

### 5.4 门户身份与当前密钥契约

身份和请求密钥分别解析。所有门户接口统一使用 `resolve_portal_principal`：有效 OIDC cookie 优先，否则验证 legacy 门户 JWT 并通过策略定位 owner；普通 sk-* API key 仍不能登录门户。cookie 与本地 JWT 属于不同用户时，以 session 返回的 principal 为准并清理旧客户端状态，界面不能显示用户 B 却请求用户 A 的数据。

扩展 `/api/portal/session` 同时支持 cookie/legacy，返回 `user`、`auth_method`、`login_downstream_id`、`default_downstream_id`、`has_keys`。新增前端 `stores/portal.ts` 保存 principal 与 selectedDownstreamId，跨页面共享，但不持久化 secret。自动初选按现有 inherit key、服务端默认 key 的顺序；用户显式选择后不被默认设置更新覆盖，密钥失效时清理选择并要求重新选择。

quota/overview/models 统计和需要隐式 key 的旧接口增加可选 downstream_id，明文读取优先使用现有按 ID 接口。明确 ID 必须先验证属于当前 principal，不能被 cookie 默认 key 替换，也不能越权后回退默认。对象响应增加 downstream_id；原数组响应保持数组，通过 X-Portal-Downstream-Id 头回显，不能为增加范围信息改变旧结构。无 ID 保持旧默认选择以兼容旧客户端。没有 key 时不请求密钥额度或明文，展示明确无密钥状态。

model-access 返回 `user_id` 和规范化模型 ID。前端分别加载用户集合与选定 key 集合，保留用户集合全部条目，用 key 集合标记能否调用，不再从旧白名单计算权限；查询失败将调用状态置为未知并禁用发送，不作为空列表或全放行。两份请求统一带选择序号，身份/密钥变化后旧响应全部丢弃。

Overview 的用户模型总数取用户范围 model-access，额度和使用统计明确为选定 key；不能继续把单 key model_summary 标成用户总数。Integration 的 secret、Codex 目录、上下文和导出配置全部绑定同一个 selectedDownstreamId，使用该 key 的新目录与元数据；旧使用统计仅用于排序，不用于授权，移除 quota allowlist 的过滤。

## 6. API 与写入契约

沿用 /api/admin/downstreams/{id}、/api/admin/downstreams/batch-update 作为兼容配置端点，页面归门户用户，不做无关路径重命名。

```json
{
  "ids": ["key-a", "key-b"],
  "updates": {
    "model_access": { "mode": "inherit" },
    "max_concurrency": 12
  }
}
```

model_access 是原子对象：inherit/deny 不带 group；group 必须带存在的 group_id。主体和 owner 由归属操作维护，普通 PATCH 不接受；解除 deny 受管理员权限约束。

| 入口 | 行为与兼容 |
|---|---|
| 单条/批量配置 | 每把密钥的配置和策略共同提交，失败不留半套；新表不被旧快照覆盖 |
| 旧 model_group_id PATCH | 合法组翻译为 group，deny-all 翻译为 deny；null/空串 400 指向显式 inherit，不能解释为放权；与 model_access 同传 400 |
| 用户模型组 PUT | basic + 显式授权集合事务替换，停止从旧绑定列反向补权 |
| 多用户授权 | 新增 /api/admin/portal/users/batch-model-groups，显式 add/remove/replace，逐用户事务/结果 |
| bindings GET | 返回归属/默认标记及派生策略，不返回旧 L2 作为生效组 |
| bindings POST/PUT | 仅管理归属/默认；旧 group 参数能无歧义翻译时同事务转到策略服务，否则 400，不能 200+忽略；省略 is_default 保持原值 |
| 门户创建密钥 | 未指定策略则 inherit；显式 group 必须已授权；配置/策略/绑定整体成功才返回 secret |
| 门户改组/轮换/删除 | 保留路径，转同一策略与生命周期服务；组信息派生，非 owner 为 403 |
| GET /api/admin/models?scope=exposed | 对外名称去重排序；前端不再拼上游及 aliases |

新增 `GET /api/admin/portal/users/access-migration` 提供摘要/待处理项；`POST /api/admin/portal/users/access-migration/apply` 接受明确选中的用户、密钥、待补组和预期 revision，执行第 4.2 节修复。预览同时返回用户授权/相关组内容的指纹，提交事务锁定对应用户、组和策略行后重新核对；任何变化均返回 409，不能按过期预览放权。

配置写入扩展现有变更锁/事务入口，所有路径保持相同锁顺序。不能在一条连接提交策略、另一条连接提交额度后统一报成功。异步组校验放服务层，纯字段合并仍保留在 apply_downstream_updates。

## 7. 总览局部刷新

- 状态拆为 ready、inFlight、manualLoading、error；防重入只看 inFlight，不依赖显示 loading。
- 在途/重试保留约 5 秒，KPI/趋势按 15 秒，模型健康按服务端间隔独立调度；请求结束后安排下次轮询，慢请求不堆积。
- 首屏可骨架；后台保留 DOM、数据、空态与表格高度。手动刷新只按钮/局部提示，不卸载现有内容。
- row-key=request_id，按稳定 ID 合并变化字段；重试排序有类别/时间二级键；数据未变不替换整份列表，数字等宽。
- 表格/空态共享稳定响应式区域高度，条数变化在区域内滚动；轮询不改变页面滚动、选中行、焦点。
- ECharts 复用实例、series 稳定 ID，只改数据并保留 legend/dataZoom，不在轮询 dispose/init 或重播整图动画。
- 每块独立序号/参数快照；切区间、卸载、隐藏时取消请求或使其失效，旧响应不能覆盖新范围。
- 隐藏暂停，恢复补刷；失败保留上次成功数据/时间，局部标记不可用，不周期性全局弹错。
- 验收实际空态/高度/响应行为，row-key 或源码出现字段名都不能代替挂载测试。

## 8. 开发拆分

以下均未实施。每项先写能复现旧行为的测试并观察预期失败，再实现、回归。提交/发布按实际开发授权执行，设计完成不代表功能已实现。

### A. 模型身份与集合基础

文件：新增 `src/state/model_catalog.rs`、`src/state/model_access.rs`；接入 `src/state.rs` 和现有 model_identity；新增 `tests/model_access_policy.rs`，扩展 `tests/gateway/model_mappings.rs`。

- [ ] 定义 ModelId/PublishedModel/AllowedModels/AccessMode/ResolvedModelAccess，复用原映射工具。
- [ ] 先验证 All/None/交并集、真改名、跨上游映射、大小写开关和 subagent。
- [ ] 实现纯函数并运行针对性测试，不改变原路由规则。

### B. 持久化与启动迁移

文件：`src/state/{postgres,portal_store,store}.rs`；新增 `migrations/2026-09-07-downstream-access-policies.sql` 留档、`tests/model_access_migration.rs`；扩展 postgres_roundtrip/portal_store_methods。

- [ ] 旧库测试覆盖第 4.1 节所有类别及幂等/回滚。
- [ ] 添加策略和审计表、事务 API，接入 initialize_schema；验证全量保存不覆盖策略。
- [ ] 实现 Eold 分类、去重组、授权补齐和待处理记录。
- [ ] 验证重复启动不覆盖新配置，删组和空归属不放权。

### C. 权限服务与读路径

文件：`src/state.rs`、model_access、`src/server/{gateway,portal,admin}.rs`、`src/state/{usage,log_queries,postgres}.rs`。

- [ ] 一致性批量解析、错误类型及无白名单回退测试。
- [ ] 同版本替换旧权限读取，删第二道 L2 闸门，接入 Codex/count_tokens/探测/上下文/统计。
- [ ] 新增 exposed 和 portal/model-access，保留原统计结构和 quota 兼容投影。
- [ ] 验证目录与真实请求同身份同权限，健康变化不改变目录。

### D. 配置、归属、批量与生命周期

文件：`src/server/{admin,portal,gateway}.rs`、`src/state/portal_store.rs`、`src/state.rs`。

- [ ] 先测 expires_at、部分 PATCH、批量部分失败、默认标记保留、授权原子替换。
- [ ] 完整字段、新策略 PATCH 和旧字段兼容翻译接入同事务服务。
- [ ] 实现单 owner、禁用/解绑/删除/轮换/兼容 admin API，验证旧 secret 失效。
- [ ] 实现迁移报告、选择性批量修复和多用户批量授权，测试 revision 冲突与回读。
- [ ] 接入 `src/server/portal_oidc.rs` 的无密钥登录及 bind intent、legacy ensure_user_for_downstream、标签修改等隐式绑定写路径，新增/扩展 `tests/portal_flow.rs`、`tests/portal_oidc.rs`、`tests/portal_api_keys_handlers.rs`。
- [ ] 接入 apply_model_qualification 的新策略和全部共享授权引用检查，扩展 `tests/model_group_qualification.rs`；原地审定不能改变其它密钥的上限。

### E. 门户用户与模型体验

文件：`frontend/src/views/admin/PortalUsers.vue`、ModelGroupForm、门户 Overview/QuotaDetails/Playground/Integration/KeyManagement、KeyCard、api/admin.ts、api/portal.ts、types、相关模型工具及 router；新增 `frontend/src/stores/portal.ts`，接入 `frontend/src/views/portal/Portal.vue`。

- [ ] Vue 挂载测试验证真实 payload/状态，不只断言源码字符串。
- [ ] 实现用户详情、全部/未关联/待处理筛选、跨用户批量及完整字段，抽出实际复用的表单/PATCH 构建器。
- [ ] 接入新模型与权限契约，显示模式和原因，新建默认 inherit，操练场不双重过滤。
- [ ] 保存回读、部分失败、抽屉切换竞态和管理员无凭据创建/轮换入口验收。
- [ ] 统一 session principal/当前密钥，校验 cookie 与 JWT 冲突、无密钥登录、Integration 导出以及概览用户/密钥统计范围。

### F. 总览刷新

文件：`frontend/src/views/admin/Dashboard.vue`、必要刷新 composable；新增 `frontend/src/views/admin/__tests__/Dashboard.refresh.spec.ts`。

- [ ] 假计时器/延迟响应先复现空态切换、请求重叠、旧范围覆盖。
- [ ] 实现独立状态、序号/取消、稳定区域、增量图表、隐藏暂停。
- [ ] Playwright 桌面/移动端连续轮询截图检查节点、高度、滚动、焦点和图表实例。

### G. 交付与文档

文件：README、DEPLOYMENT、`docs/api/model-groups.md`、`docs/multi-key-management.md`、本文实施记录。

- [ ] 修正 basic 默认、绑定组、解绑等过时文案，记录迁移和恢复步骤。
- [ ] 执行第 9 节矩阵，记录实际数量、跳过项与环境，跳过 DB 测试不计为通过。
- [ ] 留隔离库升级、实际模型请求、刷新截图证据，再标记实现完成。

依赖：A -> B -> C -> D -> E -> G；F 可独立验证。迁移、读取切换、新前端同版本发布，中间态不部署到共享数据库。代码修改及最终验证由主代理负责。

## 9. 必需验收

| 编号 | 场景 | 要求 |
|---|---|---|
| P01 | basic+premium 用户新建不传组 | inherit，目录=用户并集∩已发布模型，组外 403 |
| P02 | 多组重叠、All、显式 group | 正确去重、通配、收窄，不能串到其他用户 |
| P03 | deny、空交集、坏组、DB 失败 | 无空数组放行，确定拒绝与查询失败区分 |
| P04 | 删除限定组 | group 落 deny-all，运行中/重启后均拒绝，不变 inherit |
| P05 | 撤组、禁用、解绑最后 owner | 下一请求收缩，旧 L3=all 不变 direct 全放行 |
| P06 | 并发/历史多用户绑定 | 新冲突 409，旧冲突可见且拒绝，不并集 |
| P07 | 删除/轮换 | 旧 secret 失效，轮换保留完整配置 |
| P08 | 全部迁移类别 | 原权限按分类保留，待确认不自动开通，审计无 secret |
| P09 | 重跑/失败/升级后修改 | 幂等、不覆盖、回滚，备份恢复可执行 |
| P10 | 选定 deny-all 记录应用用户分组 | 只改所选，事务原子，门户与请求立即生效 |
| P11 | legacy 首次/重复登录、OIDC 无 key 登录/bind intent | 单 owner 一致；合规新用户能进入门户自建，不自动发 key，不依赖默认绑定放行 |
| P12 | 资格审定目标组被其它策略/用户授权引用 | 返回 conflict 且不修改；专属无共享限定组正常审定 |
| M01 | deepseek-chat -> deepseek-v3，两端分别单独授权 | 发布 canonical，按目录请求通过；不能用 B/b 替代 |
| M02 | A 上游 vendor-model 映射 public-model，B 未映射 | A 原名隐藏，B 原名保留，wire 名不扩大权限 |
| M03 | 映射失去 key 支持/权威空集合 | 无假模型、不回退账号列表；组配置保留并标无路由 |
| M04 | 普通/Codex、三协议/count_tokens、probe/context | 同主体同 ModelId 一致，不泄露组外元数据 |
| M05 | all、无路由、无密钥、受限默认密钥 | * 不字面交集；概览正确范围/空态，不显示 __none__ |
| M06 | 切换用户/密钥及撤权 | 丢弃旧响应，不错用默认 quota，不能提交无权限模型 |
| M07 | cookie A/JWT B、显式选择另一 key、越权 ID | 统一 principal；明确 ID 不被默认覆盖，越权拒绝且不回退 |
| M08 | Integration 导出/Overview 数量/旧数组响应 | 导出 key/目录/上下文一致，用户数量不误用 key 统计，旧结构不变 |
| C01 | 仅批量改并发 | 计费/Token/IP/窗口/过期等未勾选字段逐项不变 |
| C02 | 过期/IP/并发组/金额 | 单条批量一致，单位往返、null/缺失、实际生效正确 |
| C03 | 跨用户部分失败、分页/切抽屉 | 真实结果、保留失败项、无残留选择、回读新值 |
| C04 | 第 2.2 节逐字段 | 可编辑、保存、回读、运行生效，不只是控件存在 |
| R01 | 空态/有数据连续轮询及慢请求 | 无周期遮罩/节点切换、无请求堆积、滚动焦点保持 |
| R02 | 旧区间响应、隐藏/恢复、卸载 | 不覆盖新范围，暂停/补刷正确，卸载无写入 |
| R03 | 图表刷新/局部失败 | 实例/缩放/图例保留，保留成功数据，无周期全局弹错 |

完成后在仓库根目录分别执行：

```bash
rtk cargo test --test model_access_policy
rtk cargo test --test model_access_migration
rtk cargo test --workspace
rtk cargo clippy --workspace --all-targets
```

在 frontend/ 目录分别执行：

```bash
rtk npm run test
rtk npm run type-check
rtk npm run build
```

DB 测试只用隔离的 OIDC_TEST_DATABASE_URL，遵守既有测试锁。Clippy 不新增警告，记录旧基线，不为清理旧警告扩大范围。Playwright 用隔离服务/账号和可控 mock 上游，生产实跑另列；不能将静态检查或 mock 结果写成线上通过。

## 10. 实施记录

| 项目 | 状态 | 证据 |
|---|---|---|
| 设计及提示词修订 | 设计修订稿 | 本文约定权限边界、迁移、完整配置和验收，尚未实现 |
| A 模型身份与集合 | 已完成 | `tests/model_access_policy.rs` 5/5 通过（真改名 deepseek-chat→deepseek-v3、跨上游映射、alias/canonical 同身份、codex 目录无哨兵、上下文窗口） |
| B 持久化与迁移 | 已完成（DB 测试 16/16） | `migrations/2026-09-07-downstream-access-policies.sql` + `model_access_store.rs`；`tests/model_access_migration.rs` 16/16（全分类迁移、legacy 登录建档、轮换/删除、回滚原子性、P10 预览/apply/409、批量授权、5.4 scope owner 校验）。迁移用空 alias 目录，等价拼写差异按 review_required 收敛（fail-safe，见下「迁移待处理记录」） |
| C 统一读取 | 已完成 | gateway 四条路径 + codex 目录 + usage 经 `resolved_model_access`/`ModelCatalog`（`tests/gateway` 452/452）；`portal_api` 39/39；quota/models/overview/probe 支持显式 `downstream_id`+owner 校验（越权 403 不回退，数组响应保持并经 `X-Portal-Downstream-Id` 回显） |
| D 配置与生命周期 | 已完成 | `downstream_patch.rs` 完整字段 PATCH（expires_at 秒/null 清空）；`apply_model_qualification` 改读策略表（P12 6/6，用户授权引用拒绝且不修改）；access-migration 预览/apply 接口、batch-model-groups 接口（逐用户事务/部分失败）；门户 create/rotate/delete/解绑/轮换全生命周期接入策略；`session` 扩展 cookie/legacy 双身份 |
| E 前端体验 | 已完成（331 前端测试通过） | `stores/portal.ts`（principal+selectedDownstreamId，显式选择不被默认覆盖、失效清理）；KeyManagement/KeyCard 新密钥默认 inherit+model_access 交互；Overview/Playground/Integration 改用 model-access 契约（不再 quota 白名单二次过滤 /v1/models）；PortalUsers 绑定行展示+编辑配置 model_access、批量部分失败回读、迁移修复入口、跨用户批量授权；`stores/portal.spec`、KeyCard/KeyManagement/portal.spec 更新 |
| F 总览刷新 | 已完成（单元级，未浏览器验收） | `Dashboard.refresh.spec.ts` 3/3：防重入不堆积、乱序响应丢弃、失败保留数据+error 状态；轮询改为完成后安排（不再 setInterval 死循环）、visibilitychange 暂停/恢复补刷、row-key 稳定合并、主题切换保存/恢复图例与缩放。**Playwright 截图未做**（仓库无 Playwright 基建且未真实部署，见下） |
| G 完整交付 | 后端+前端全量回归通过 | `cargo test --workspace`（含隔离 DB）全绿、clippy 无新增警告（15 处旧基线记录）、`npm run test` 331/331、`npm run type-check` 通过（含修复 2 处既有类型错误）、`npm run build` 通过；本实现未提交（见交付说明「提交」列） |
