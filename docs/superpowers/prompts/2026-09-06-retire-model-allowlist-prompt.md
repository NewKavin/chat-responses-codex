项目：/home/kavin/projects/chat2Responses（Rust 网关，chat↔responses 协议转换 + 多上游路由）

任务：让"模型分组"成为下游模型权限的唯一事实源，最终下线 model_allowlist。完整方案见
docs/superpowers/plans/2026-09-06-retire-model-allowlist.md，先完整读一遍再动手。

分四个阶段，一次只做一段。**做完阶段 2 必须停下等我确认**（线上要观察 1 周），确认后再做
阶段 3；阶段 3 之后再观察 2 周才做阶段 4。不要一口气做到底。

三项决策已拍板，不要再问、也不要改：
  1) 默认权限档 = sentinel 组 deny-all（allowed_models = ["__none__"]）
  2) basic / premium 的占位种子模型改成本部署真实模型
  3) apply_model_qualification 改为写入下游所绑分组的 allowed_models

================================ 强制流程 ================================

项目铁律，见 CLAUDE.md：
- TDD。改任何生产代码前先写测试，运行它，亲眼看到它因"功能不存在"而失败，再写最小实现，
  最后重构。代码先写、测试后补 = 删掉重来。
- 所有命令加 rtk 前缀：rtk cargo test / rtk cargo clippy / rtk git diff，命令链里每段都要加。
- 动之前先读代码确认根因，不要猜着改。同一方向失败两次就停下说明诊断错了。
- 删字段、删日志、删诊断信息前先确认没人依赖。不确定就问我。
- 任何一处你认为方案写错了，先说出来再改，不要静默偏离。

================================ 背景事实 ================================

已核实的代码事实，file:line 为证：
- 已经看分组的读路径只有 3 处：src/server/gateway.rs:5570（主请求）、:3282（count_tokens）、
  src/state.rs:6220（available_models_for_downstream）。三处是同一段 40 行逻辑的复制粘贴。
- 不看分组、只读 model_allowlist 的读路径有 11 处，全是 bug：
  src/server/gateway.rs:3056、src/server/portal.rs:255、:498、src/state/usage.rs:573、:635、
  src/state/log_queries.rs:219,238、src/state/postgres.rs:637,674、src/state.rs:6145、:6298、
  :6733、crates/gateway-core/src/admin.rs:247。
- 最关键的是 gateway.rs:3056（list_models_codex_format）。它就是 /v1/models?format=codex，
  Codex 客户端的 ~/.codex/model-catalog.json 整份来自这个接口。目录错了 Codex 会在启动阶段
  报错（config.toml 的 model 必须精确等于某个目录 slug），或选到必然被主路径 403 的模型。
  子代理 ~/.codex/agents/default.toml 独立加载同一份目录，更敏感。这一处优先修。
- 白名单匹配永远大小写不敏感：portal_model_is_allowed（src/state/usage.rs:247）无条件
  to_ascii_lowercase，与 model_case_insensitive_matching 开关无关。不要为此加判断。
- model_list_allows（usage.rs:275）把空列表当"放行全部"、"*" 当通配。这条语义全程不要改，
  这也是为什么"拒绝全部"必须用 deny-all 的 ["__none__"] 而不是空数组。
- ModelGroup::allows_model（src/state/portal_store.rs:83）是精确匹配、大小写敏感，目前只在
  测试里用。不要把它接到请求路径上。
- model_groups.id 有 CHECK (id ~ '^[a-z0-9-]+$')，自动生成的组 id 必须小写；allowed_models
  的值没有该约束，要保留原始拼写。
- 两层分组都在生效：key 级 downstreams.model_group_id（gateway.rs:5570），绑定级
  portal_user_downstreams.model_group_id（gateway.rs:5664 经 portal_store.rs:1153，NULL 时
  fail-closed 落 basic）。本次只动 key 级。

================================ 阶段 0：修种子数据 ================================

种子数据写在两个地方，只改一个不够：src/state/postgres.rs:2113 的 SCHEMA_SQL（启动时执行，
带 ON CONFLICT DO NOTHING，只影响新库），和 migrations/2026-09-03-add-model-groups.sql
（历史留档，不再重跑）。既有库靠一条新 UPDATE migration 修，新库靠改 SCHEMA_SQL 修。

必须先做的理由：绑定级闸门默认就落 basic，而 basic 现在存的是 gpt-3.5-turbo /
claude-3-haiku 这种占位模型，与本部署真实模型零交集，门户新建的 key 这一层已经在封人。
这是既有故障，跟下线白名单无关。

T0-1 新建 migrations/2026-09-06-fix-model-group-seeds.sql，内容照抄方案 §3.0：
  - 建 deny-all 组，allowed_models = ["__none__"]
  - basic 改成 deepseek-v4-flash / deepseek-v4-flash-0731 / deepseek-v4-flash-free /
    glm-5.3-flash / kimi-k3
  - premium 改成 GLM-5.2 / glm-5.3 / deepseek-v4-pro / deepseek-v4-pro-0813 / gpt-5.5 /
    gpt-5.6-luna / gpt-5.6-sol / gpt-5.6-terra / grok-4.5 / grok-4.6 / claude-fable-5 /
    claude-opus-5 / claude-opus-4-8 / claude-sonnet-5 / qwen3.8-max
  上面这份清单是按本机 Codex 目录推的分档，**执行前用 GET /admin/models 复核**，不在 active
  upstream 里的 slug 删掉，改动后的清单报给我。migration 必须幂等。
T0-2 同步改 SCHEMA_SQL 的种子，让新库初始化出来的四个组与 migration 完全一致。
  ON CONFLICT DO NOTHING 保持不动，别改成 upsert（那会在每次启动时覆盖运维的手工调整）。
T0-3 备份 pg_dump -t downstream_model_allowlist -t downstreams -t model_groups；
  对每个在用 key 存一份 /v1/models?format=codex 的 slug 快照。

验收：SELECT id, name, allowed_models FROM model_groups ORDER BY id 四个组符合预期；
门户新建一个 basic key，实跑一次会话能通。

============================ 阶段 1：补齐读路径（纯修 bug）============================

按方案 §3.1 表格逐处改造，7 个 TDD 任务 T1–T7，独立可发布。

T1 在 src/state.rs 加 AppState::effective_model_allowlist(&self, &DownstreamConfig)
   -> Result<Vec<String>, String>。无组回退白名单；有组走 get_allowed_models；无 portal
   store 回退白名单；Err 原样透传由调用方决定。
T2 gateway.rs:5570 / :3282 / state.rs:6220 三处改调用 T1，各自保留现有错误响应格式。
T3 Codex 目录走分组（gateway.rs:3056），fail-closed。优先做。
T4 portal_quota（portal.rs:255）/ portal_model_probe（:498）走分组。
T5 compute_model_stats（usage.rs:573）/ compute_portal_model_context_limits（:635）走分组。
T6 两套 usage summary 走分组：log_queries.rs:219,238（文件模式）与 postgres.rs:637,674
   （DB 模式，SQL 直查 downstream_model_allowlist 表）。
T7 state.rs:6145 / :6298 / :6733 走分组。

判据：权限/写路径 Err 必须 fail-closed（沿用错误码 model_group_check_failed）；纯展示统计
路径 Err 降级为空列表 + warn，不要 500 掉报表接口。

注意：
- state.rs:6145/6298/6733 是循环体，逐 downstream 查组会 N+1。先按 model_group_id 去重批量
  查一次再进循环。
- log_queries.rs 是纯函数拿不到 store，签名加 effective_allowlist: &[String] 参数，调用侧
  解析后传入。postgres.rs:637,674 改成参数化传入已解析的清单。
- crates/gateway-core/src/admin.rs:247 在 crate 边界拿不到 store，加 TODO 保留原样。
- portal.rs:255 只改字段的值，字段名 model_allowlist 保持不变（门户四个页面
  Overview/QuotaDetails/Playground/Integration 依赖这个名字，前端不动）。

============================== 阶段 2：迁移历史 key ==============================

按方案 §3.2。**必须在阶段 1 的代码上线之后才跑**（新代码兼容有组/无组两种状态，老代码不认识
新读路径）。

空白名单挂 all；非空按内容去重建 auto-<sha256(小写排序后拼接)[..8]> 组，allowed_models 存原
拼写、按小写去重。SQL 写到 migrations/2026-09-06-migrate-model-allowlist-to-groups.sql，
必须幂等。SHA256 参数用 CONVERT_TO(..., 'UTF8')，不要 ::bytea 强转（非 ASCII 会报错）。
附上方案里的前后对比校验查询，跑出 0 行的证据。

验收：downstreams 里 model_group_id IS NULL 的行数 = 0；对比查询 0 行；每个在用 key 的
Codex 目录 diff 无差异；抽查 3 个 key（含一个 Codex key 实跑一次会话）。

>>>>>>>> 阶段 2 做完停下，把证据发我，等我确认后再继续 <<<<<<<<

============================ 阶段 3：停写 model_allowlist ============================

数据仍在，可回滚。T10–T13。

T10 src/server/admin.rs:2302、src/state.rs:8401 移除写入分支，改为忽略 + warn（老客户端传值
    时返回 200 不报错）；admin.rs:2709 从 BATCH_UPDATE_DOWNSTREAM_ALLOWED_FIELDS 移除
    "model_allowlist"。postgres.rs:1417-1433 暂不动，继续写表保留回滚数据。
T11 src/state.rs:7230 apply_model_qualification 现在把资格审定结果写进目标下游的
    model_allowlist，这是唯一的功能性写入。改成写入该下游**所绑分组**的 allowed_models，
    并且必须同时实现三条防外溢规则，否则单个下游的审定会改掉别人的权限：
      1) 下游未绑组 → 返回 InvalidInput，不要退回去写 model_allowlist，也不要自动建组；
      2) 所绑组是内置组（all / basic / premium / deny-all）→ 拒绝，提示"请先改绑到专属分组"；
      3) 所绑组被 2 个及以上下游引用
         （SELECT COUNT(*) FROM downstreams WHERE model_group_id = $1）→ 同样拒绝。
    写入用 UPDATE model_groups SET allowed_models = $2, updated_at = NOW() WHERE id = $1，
    与下游快照更新放在同一事务；如果阶段 1 加了组缓存，一并失效。
T12 新建下游 model_group_id 必填，未指定时落 deny-all。测试要断言用该 key 请求任何模型返回
    403，不要只断言字段值。
T13 前端：frontend/src/views/admin/Downstreams.vue 删掉 modelManagementMode 的 manual 选项
    (:329-340)、手动白名单分支 (:396)、两个 watch (:868-882)，表格列 (:77-81) 只显示分组；
    frontend/tests/views/admin-ui.spec.ts:237 的断言跟着改；types/index.ts:192 标
    @deprecated。门户四个页面不动。scripts/redis_runtime_smoke.sh:360 的 fixture 跟着更新。

>>>>>>>> 阶段 3 做完停下，观察 2 周，等我确认后再继续 <<<<<<<<

============================== 阶段 4：删列删表 ==============================

T14–T16，顺序不能颠倒。

T14 先把约 76 个测试文件里显式写 model_allowlist 的 DownstreamConfig 夹具统一改成
    ..Default::default()（其中 62 个已经在用），跑全量测试绿。否则删字段时编译错误会淹没
    真正的回归。
T15 再落权限兜底。现在 model_group_id NULL 回退白名单、空白名单 = 全放行，删掉白名单后
    NULL 组会直接变"无限制"，这是权限放大。deny-all 组阶段 0 已建，本阶段只改约束：
      ALTER TABLE downstreams DROP CONSTRAINT fk_downstream_model_group;
      ALTER TABLE downstreams
        ALTER COLUMN model_group_id SET DEFAULT 'deny-all',
        ALTER COLUMN model_group_id SET NOT NULL;
      ALTER TABLE downstreams ADD CONSTRAINT fk_downstream_model_group
        FOREIGN KEY (model_group_id) REFERENCES model_groups(id) ON DELETE SET DEFAULT;
    必须有 DB 测试证明：删掉某组后原绑定下游落到 deny-all，请求任何模型 403 而不是放行。
    不要改 model_list_allows 的空列表语义（那个备选方案已被否掉）。
T16 最后删字段与表：src/state/types.rs:1135 删字段、:1326-1355 get_allowed_models 简化为
    直接读组、src/state/postgres.rs:188/226-238/1417-1433 删读写、:1930 从 SCHEMA_SQL 删
    建表、AppState::effective_model_allowlist 去掉回退分支、
    crates/gateway-core/src/admin.rs:247 改用组、src/state/types_model_group_tests.rs 的两个
    falls_back_to_model_allowlist_* 用例删除。删表前先
    pg_dump -t downstream_model_allowlist 备份。

================================ 测试与验收 ================================

每个阶段都要跑：
  rtk cargo test          # DB 用例需 OIDC_TEST_DATABASE_URL，skip 写法参考 tests/model_groups_migration.rs
  rtk cargo clippy        # 不许新增 warning
  cd frontend && rtk vitest run

重点套件：tests/downstream_model_groups.rs、tests/gateway/model_permission_validation.rs、
tests/portal_api.rs、tests/downstream_quota.rs、tests/troubleshooting.rs、
tests/capability_probe.rs、tests/postgres_roundtrip.rs、tests/model_groups_migration.rs、
tests/admin_downstreams.rs、tests/admin_upstreams.rs。

Codex 端到端验收，每个阶段都要做，不许跳：
  # 变更前后各对每个在用 key 跑一次
  curl -s -H "Authorization: Bearer <key>" "http://<gateway>/v1/models?format=codex" \
    | jq -S '[.models[].slug]|sort'
  # 未绑组的 key 必须逐位无差异；已绑组的 key 差异必须与组内容一致
  jq -r '.models[].slug' ~/.codex/model-catalog.json   # 仍包含 ~/.codex/config.toml 的 model 值
  codex --strict-config doctor --summary

交付：每个任务回填 commit 号与 ✅ 状态到方案文档对应表格；阶段 0 报出复核后的真实模型清单；
阶段 2 附上校验查询的实际输出和 Codex diff 结果。
