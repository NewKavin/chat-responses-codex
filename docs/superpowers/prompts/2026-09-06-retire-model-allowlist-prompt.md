项目：/home/kavin/projects/chat2Responses（Rust 网关，chat↔responses 协议转换 + 多上游路由）

任务：让"模型分组"成为下游模型权限的唯一事实源，下线 model_allowlist。完整方案见
docs/superpowers/plans/2026-09-06-retire-model-allowlist.md，先完整读一遍再动手，特别是 §6.5。

部署形态是硬约束：**内网通过 tar 包升级，没有"发版 → 手工跑 SQL → 再发版"的多窗口条件。**
所以这次要做成一次性部署：一个 tar 包起来，种子修正、通配符修复、读路径改造、数据迁移、
停写、权限兜底全部自动生效。只有"删列删表"留到下一个版本。

三项决策已拍板，不要再问、也不要改：
  1) 默认权限档 = sentinel 组 deny-all（allowed_models = ["__none__"]）
  2) basic / premium 的占位种子模型改成本部署真实模型
  3) apply_model_qualification 改为写入下游所绑分组的 allowed_models

已完成的部分（不用重做，但要读代码确认现状）：
  3324794f 阶段 0：deny-all 组 + 真实模型种子进 SCHEMA_SQL + migration 文件
  84fd375f 阶段 1：读路径走分组（effective_model_allowlist 在 src/state.rs，11 处调用点已改）
  afae8249 阶段 1：批量解析消 N+1 + 测试
  dbf6cdb8 阶段 2：迁移 SQL 文件 + scripts/catalog-diff.sh

================================ 强制流程 ================================

项目铁律，见 CLAUDE.md：
- TDD。改任何生产代码前先写测试，运行它，亲眼看到它因"功能不存在"而失败，再写最小实现，
  最后重构。代码先写、测试后补 = 删掉重来。
- 所有命令加 rtk 前缀：rtk cargo test / rtk cargo clippy / rtk git diff，命令链里每段都要加。
- 动之前先读代码确认根因，不要猜着改。同一方向失败两次就停下说明诊断错了。
- 删字段、删日志、删诊断信息前先确认没人依赖。不确定就问我。
- 任何一处你认为方案写错了，先说出来再改，不要静默偏离。

============================ 两个阻塞项，先解决 ============================

### 阻塞项 1：migrations/*.sql 没有执行器

全仓库没有任何代码读取并执行 migrations/ 目录：
  grep -rn "migrations/" --include=*.rs --include=*.sh --include=Dockerfile* .
  → 只有注释和测试引用

唯一自动执行的 SQL 是 SCHEMA_SQL（src/state/postgres.rs:1788），由
initialize_schema（:911）在启动时 batch_execute。所以 tar 包升级场景下，
migrations/2026-09-06-*.sql 这两个文件**根本不会被执行**。

修法（方案 §6.5.2）：在 initialize_schema 里追加一个幂等迁移步骤，与既有的
migrate_dialect_profiles_primary_key / migrate_response_history_primary_key 同层：

    async fn initialize_schema(&self) -> io::Result<()> {
        let mut conn = self.pool.get().await.map_err(io_other)?;
        let tx = conn.transaction().await.map_err(io_other)?;
        tx.batch_execute(SCHEMA_SQL).await.map_err(io_other)?;
        migrate_dialect_profiles_primary_key(&tx).await?;
        migrate_response_history_primary_key(&tx).await?;
        migrate_model_allowlist_to_groups(&tx).await?;   // 新增
        tx.commit().await.map_err(io_other)
    }

migrate_model_allowlist_to_groups 的内容 = migrations/2026-09-06-migrate-model-allowlist-to-groups.sql
的 SQL（去掉 BEGIN/COMMIT，它跑在外层事务里），加两条防护：
  1) 幂等：只处理 model_group_id IS NULL 的行，重启 N 次结果一致
  2) 前置检查：downstream_model_allowlist 表不存在时直接跳过（下个版本删表后的重启路径）

migrations/ 下的 .sql 文件保留作留档，文件头加注释说明"已在启动时自动执行，无需手工运行"。

### 阻塞项 2：既有库的 basic/premium 不会被修正

3324794f 把真实模型种子加进了 SCHEMA_SQL，但那段是
INSERT ... ON CONFLICT (id) DO NOTHING。既有库里 basic/premium 已存在，DO NOTHING 会跳过，
**占位模型 gpt-3.5-turbo / claude-3-haiku 不会被改掉**。而绑定级闸门
（gateway.rs:5664 经 portal_store.rs:1153）默认就落 basic，门户新建的 key 现在这一层在封人。

修法（方案 §6.5.4）：在 SCHEMA_SQL 里补一段带条件的 UPDATE，只在内容仍是初版占位值时才改，
这样幂等且不覆盖运维后来的手工调整：

    UPDATE model_groups
    SET allowed_models = '["deepseek-v4-flash", ...]'::jsonb, name = 'Basic Models', updated_at = NOW()
    WHERE id = 'basic' AND allowed_models = '["gpt-3.5-turbo", "claude-3-haiku"]'::jsonb;
    -- premium 同理，条件是 '["gpt-4", "gpt-4-turbo", "claude-3-opus", "claude-3.5-sonnet", "claude-3-sonnet"]'

完整 SQL 见方案 §6.5.4。同时把 migrations/2026-09-06-fix-model-group-seeds.sql 里那两条
**无条件 UPDATE 改成带条件版本**（当前那份会覆盖运维调整）。

============================== 通配符缺口（仍未修）==============================

model_list_allows（src/state/usage.rs:275）认 "*"，但它的两个下游消费者都不认。
已核实截至目前**这个缺口还开着**：codex_exposed_models 里只有 `allowlist.is_empty()`
（gateway.rs:2995），没有 "*" 判断。

缺口 1：codex_exposed_models（src/server/gateway.rs:2953）
传入 allowed_models = ["*"] 的执行路径：
  is_empty() = false          → 进非空分支（:3004 附近）
  allowed_slugs = {"*": "*"}  → 拿 "*" 和 upstream 模型逐个字面比对
  无一匹配                     → matched_allowlist_keys 为空
  未匹配项照原样 push          → exposed = ["*"]
结果 /v1/models?format=codex 只返回一个名叫 * 的模型。~/.codex/config.toml 的 model 值不在
目录里，Codex 启动阶段直接报错，请求到不了网关；子代理 agents/default.toml 同样加载失败。

缺口 2：portal_model_is_allowed（src/state/usage.rs:247）函数体内没有 "*" 分支
（只有 is_empty() 早返回），["*"] 被当成"只允许一个字面叫 * 的模型"。当前仍在直接调用它的点：
  src/server/admin.rs:1263      → 模型探测 retain 把所有模型滤掉，结果全空
  src/state/usage.rs:586, :675  → 模型统计 / 上下文清单为空
  src/state/log_queries.rs:238  → active_models 恒为 0
  src/state.rs:6158, :6401, :6850 → 判定"未对任何下游暴露"，探测不排队、scope=visible 为空

为什么现在没炸：all 组目前只被 portal_user_downstreams 那层用，那层走 model_list_allows
（gateway.rs:5675）认 "*"。downstreams.model_group_id 侧还没有下游绑 all。
**迁移一跑（空白名单 → all 组）就会全面触发。**

修法：
- codex_exposed_models 开头让 ["*"] 和空列表走同一分支：
    if allowlist.is_empty() || allowlist.iter().any(|a| a.trim() == "*") { /* 现有 is_empty 分支 */ }
- 上面列的 portal_model_is_allowed 调用点统一改调 model_list_allows（它内部先处理 "*" 再
  委托给 portal_model_is_allowed）。不要去改 portal_model_is_allowed 本身，它是"精确成员
  判定"语义，别的地方还依赖，改它风险更大。

RED 测试：给某下游绑 all 组，断言 /v1/models?format=codex 返回所有 active upstream 模型
（不是一个 *）；门户配额、模型探测、scope=visible 三处同样返回全量。

================================ 背景事实 ================================

已核实的代码事实，file:line 为证：
- 白名单匹配永远大小写不敏感：portal_model_is_allowed 内部 normalize_model_name（usage.rs:239）
  无条件 to_ascii_lowercase，与 model_case_insensitive_matching 开关无关。不要为此加判断。
- model_list_allows 把空列表当"放行全部"、"*" 当通配。这条语义全程不要改，这也是为什么
  "拒绝全部"必须用 deny-all 的 ["__none__"] 而不是空数组。
- ModelGroup::allows_model（src/state/portal_store.rs:83）是精确匹配、大小写敏感，目前只在
  测试里用。不要把它接到请求路径上。
- model_groups.id 有 CHECK (id ~ '^[a-z0-9-]+$')，自动生成的组 id 必须小写；allowed_models
  的值没有该约束，要保留原始拼写。
- 两层分组都在生效：key 级 downstreams.model_group_id（gateway.rs:5570），绑定级
  portal_user_downstreams.model_group_id（gateway.rs:5664 经 portal_store.rs:1153，NULL 时
  fail-closed 落 basic）。本次只动 key 级。
- premium 组里 glm-5.2 是小写，live 目录里是 GLM-5.2。匹配走 model_list_allows 大小写不敏感
  能命中，Codex 目录输出的拼写来自 upstream 侧仍是 GLM-5.2，不影响使用。不用改。

============================ 本次 tar 包要交付的内容 ============================

按下面顺序做，每项都是一轮 TDD。

W1  通配符缺口 1：codex_exposed_models 认 "*"
W2  通配符缺口 2：上面列的 8 个 portal_model_is_allowed 调用点改调 model_list_allows
M1  阻塞项 2：SCHEMA_SQL 补带条件的 basic/premium UPDATE；
    migrations/2026-09-06-fix-model-group-seeds.sql 的无条件 UPDATE 改成带条件版本
M2  阻塞项 1：migrate_model_allowlist_to_groups 进 initialize_schema，幂等 + 表不存在时跳过
M3  启动迁移的日志（方案 §6.5.6）。三种情况都要有输出，无变化时也要打一行，
    否则内网无法区分"跑了且无事可做"和"根本没跑"：
      info: model allowlist migration: 12 downstreams → all group, 5 auto groups created, 23 bound
      info: model allowlist migration: nothing to migrate (all downstreams already grouped)
      warn: model allowlist migration: table downstream_model_allowlist absent, skipping
T10 停写：src/server/admin.rs:2302、src/state.rs:8518 移除写入分支改为忽略 + warn（老客户端
    传值时返回 200 不报错）；admin.rs:2709 从 BATCH_UPDATE_DOWNSTREAM_ALLOWED_FIELDS 移除
    "model_allowlist"。postgres.rs:1417-1433 **暂不动**，继续双写保留回滚数据。
T11 src/state.rs:7346 apply_model_qualification 现在把资格审定结果写进目标下游的
    model_allowlist，这是唯一的功能性写入。改成写入该下游**所绑分组**的 allowed_models，
    必须同时实现三条防外溢规则，否则单个下游的审定会改掉别人的权限：
      1) 下游未绑组 → 返回 InvalidInput，不要退回去写 model_allowlist，也不要自动建组
      2) 所绑组是内置组（all / basic / premium / deny-all）→ 拒绝，提示"请先改绑到专属分组"
      3) 所绑组被 2 个及以上下游引用
         （SELECT COUNT(*) FROM downstreams WHERE model_group_id = $1）→ 同样拒绝
    写入用 UPDATE model_groups SET allowed_models = $2, updated_at = NOW() WHERE id = $1，
    与下游快照更新放在同一事务；阶段 1 若加了组缓存，一并失效。
T12 新建下游 model_group_id 必填，未指定时落 deny-all。测试要断言用该 key 请求任何模型返回
    403，不要只断言字段值。
T13 前端：frontend/src/views/admin/Downstreams.vue 删掉 modelManagementMode 的 manual 选项
    (:329-340)、手动白名单分支 (:396)、两个 watch (:868-882)，表格列 (:77-81) 只显示分组；
    frontend/tests/views/admin-ui.spec.ts:237 的断言跟着改；types/index.ts:192 标 @deprecated。
    门户四个页面不动。scripts/redis_runtime_smoke.sh:360 的 fixture 跟着更新。
T14 把约 76 个测试文件里显式写 model_allowlist 的 DownstreamConfig 夹具统一改成
    ..Default::default()（其中 62 个已经在用）。这一步现在做，不留到下个版本，否则下次删字段
    时编译错误会淹没真正的回归。
T15 权限兜底进 SCHEMA_SQL（用 DO $$ 块包幂等判断，仿照 postgres.rs:2106 那段 FK 的写法）：
      ALTER TABLE downstreams DROP CONSTRAINT fk_downstream_model_group;
      ALTER TABLE downstreams
        ALTER COLUMN model_group_id SET DEFAULT 'deny-all',
        ALTER COLUMN model_group_id SET NOT NULL;
      ALTER TABLE downstreams ADD CONSTRAINT fk_downstream_model_group
        FOREIGN KEY (model_group_id) REFERENCES model_groups(id) ON DELETE SET DEFAULT;
    注意 SET NOT NULL 之前必须确保迁移已把所有 NULL 填完，所以这段要排在
    migrate_model_allowlist_to_groups 之后。必须有 DB 测试证明：删掉某组后原绑定下游落到
    deny-all，请求任何模型 403 而不是放行。不要改 model_list_allows 的空列表语义。

**不要做 T16（删 model_allowlist 字段、删 downstream_model_allowlist 表）。**
表留着是回滚路径（方案 §6.5.7），下个版本再删。

================================ 测试与验收 ================================

  rtk cargo test          # DB 用例需 OIDC_TEST_DATABASE_URL，skip 写法参考 tests/model_groups_migration.rs
  rtk cargo clippy        # 不许新增 warning
  cd frontend && rtk vitest run

重点套件：tests/downstream_model_groups.rs、tests/gateway/model_permission_validation.rs、
tests/portal_api.rs、tests/downstream_quota.rs、tests/troubleshooting.rs、
tests/capability_probe.rs、tests/postgres_roundtrip.rs、tests/model_groups_migration.rs、
tests/admin_downstreams.rs、tests/admin_upstreams.rs。

新增必须有的测试：
- 绑 all 组时 Codex 目录 / 配额 / 探测 / scope=visible 四处均返回全量（通配符回归网）
- 启动迁移幂等：连续两次 initialize_schema 结果一致
- 启动迁移在 downstream_model_allowlist 表缺失时跳过而不报错
- 既有库场景：basic 内容为占位值时被 UPDATE 修正；已被运维改过时不动
- T15：删组后下游落 deny-all 且请求 403

模拟一次真实升级（不能只靠单测）：
  1) 起一个装了旧 schema + 占位 basic + 若干带白名单下游的库
  2) 用新代码启动，看启动日志的迁移行
  3) 核对 model_group_id 全非空、basic 已修正、Codex 目录 diff 符合预期
  4) 再重启一次，确认日志变成 "nothing to migrate" 且数据不变

Codex 端到端验收：
  scripts/catalog-diff.sh snapshot before   # 升级前
  # ...升级...
  scripts/catalog-diff.sh snapshot after && scripts/catalog-diff.sh diff
  # 未绑组的 key 必须逐位无差异；挂到 all 组的 key 必须是全量模型而不是一个 *
  jq -r '.models[].slug' ~/.codex/model-catalog.json   # 仍包含 ~/.codex/config.toml 的 model 值
  codex --strict-config doctor --summary

================================ 交付 ================================

- 每项回填 commit 号与 ✅ 状态到方案文档对应表格
- 附上模拟升级的启动日志（两次重启的日志都要）
- 附上 §2.3 校验查询的实际输出（期望 0 行）和 catalog-diff 结果
- 写一份给运维的升级说明：备份命令、快照命令、启动后要核对什么、§6.5.7 的回滚 SQL
