# 方案：瞬态 502 路由耗尽自愈优化（Part A）+ 上下游模型名映射归一（Part B）

日期：2026-08-13
状态：Part A 已完成；Part B 待开发

## 任务回填（commits，branch `part-a`）

| 任务 | commit |
|------|--------|
| A1 请求内冷却升级抑制 | `7e37973`（feat）+ `a1082ae`（fix 跨轮抑制）+ `a1b8a55`（test 字段计数） |
| A2 预算对齐的最后等待 | `3e93ba5`（feat） |
| A3 全冷却时 last-resort 半开探测 | `3441077`（registry/reserve）+ `40c1cf2`（runtime 开关）+ `34ed543`（gateway 集成） |
| A4 参数与部署文档 | `626b9dc`（DEPLOYMENT.md Intranet 小节） |
| A5 可观测性 | `0609d1f`（give_up_reason / StreamDiagnostics / dashboard 分类） |

Part B 未动；分支基准为 `73fbdee`（本计划文档落地提交）。

关联报错（内网聚合网关部署，2026-08-12 共模熔断方案落地之后仍出现）：

```
stream disconnected before completion: [upstream_routes_exhausted] all eligible
upstream routes are temporarily unavailable: transient upstream server errors
(3 routes, upstream HTTP 502); please try again in 14s; gateway already retried
for 6.8s across 3 routing rounds
```

---

# Part A：瞬态 502 路由耗尽自愈优化

## 一、报错来源与完整链路（已核实代码）

1. 错误文本产生于 `terminal_route_failure_error`（`src/server/gateway/errors.rs:79-160`）
   的 `TerminalFailure::Temporary` 分支：code=`upstream_routes_exhausted`，
   `please try again in Ns` 的 N 取健康注册表的 live earliest recovery
   （`src/server/gateway.rs:7262-7274` 调用点），`gateway already retried for X across
   N rounds` 来自 `RouteRetryBudget`（waited/round 计数）。
2. 路由主循环 `'routing_rounds`（`gateway.rs:5324`）：每轮遍历 协议 × 上游 × key；
   处于冷却/半开占用的路由**直接跳过、不发请求**（`gateway.rs:5741`
   `RouteAvailability::Cooling → record_cooled_route_attempt`）。
3. 轮间等待由 `RouteRetryPolicy::decide` 决定（`src/server/gateway/route_retry.rs:114-164`），
   两个放弃条件：
   - `budget.current_round >= max_rounds`（默认 3，`DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_ROUNDS`，`src/state/types.rs:101`）→ 直接 None；
   - 下一次需要的 sleep 超出剩余时间预算（默认 30s，`...MAX_WAIT_MS`，`types.rs:100`）→ None。
   两者都是 runtime settings，可在管理页调整。
4. 路由失败冷却：`route_cooldown`（`src/state/route_health.rs:1408-1425`）。
   `TransientServer` 使用可配置 base（默认 10s）/max（默认 300s），指数升级
   `jittered_backoff(base << (step-1))`；step 来自 `failure_step`
   （`route_health.rs:1380-1406`）：10 分钟窗口内同类连续失败逐次 +1。
   `EdgeProxyError` 固定 3s、永不升级。
5. 流式路径的终态错误以 SSE `response.failed` + `error` 事件下发
   （`src/server/gateway/stream.rs:200-267`）；codex 端显示为
   `stream disconnected before completion: ...`，并按 message 里的
   `please try again in Ns` 做退避重试。

## 二、本次报错的量化复盘

报错三个数字反推（默认配置下自洽）：

- **“3 routing rounds”= 默认 `max_rounds=3` 打满**。放弃是轮数触顶，不是预算耗尽：
  30s 预算只用了 6.8s，剩 23.2s。
- **放弃那一刻网关明知 14s 后有路由恢复**（terminal 消息里的 14s 正是 live earliest
  recovery），且 14s < 23.2s 剩余预算——“明知等得到，却因轮数上限放弃”。
- **14s 是升级后的冷却剩余**：每一轮网关内重试失败都会给同一批路由 step+1，
  冷却指数翻倍（10→20→40s，或调低 base 后等比例）。加上 codex 客户端自身的
  外层重试（每次重试进来又烧一轮 step），冷却被持续推高。确切数值取决于此前
  请求留下的 streak 状态，但机制是确定的。
- **内网单聚合网关形态**：3 条“路由”物理上是同一跳（同一 new-api/one-api 网关）。
  网关瞬断 → 同轮全部 502。而 2026-08-12 方案的任务 2 刻意让**同 host 失败不进
  transient 共模 streak**（防误判，正确），副作用是共模瞬态分支的“冷却回滚 +
  延迟重放”自愈路径在内网形态下**永远不会触发**，请求只能走通用耗尽路径。

## 三、根因归纳

| # | 根因 | 后果 |
|---|------|------|
| R1 | **自我放大**：同一下游请求的多个 routing round（以及客户端的紧凑外层重试）反复给同一批路由升级冷却 step | 2-3s 的真实瞬断被放大成几十秒的池级不可用，retry_after 越报越长 |
| R2 | **约束错位**：轮数上限（3）先于时间预算（30s）生效，且放弃时不看 live recovery 是否在剩余预算内 | 明明再等 14s 就恢复，却提前放弃并把等待转嫁给客户端 |
| R3 | **全冷却窗口内零探测**：冷却中的路由被无条件跳过，请求 0 物理尝试直接失败 | 上游实际已恢复时，网关要等冷却钟走完才知道；恢复延迟=冷却时长而非真实故障时长 |
| R4 | **内网形态下共模瞬态自愈失效**：同 host 不计 streak → 冷却回滚/延迟重放分支不可达 | 最需要“识别共享跳瞬断”的部署形态反而没有该能力 |
| R5 | （次要）默认参数偏公网：transient base 10s、max_rounds 3 对内网聚合网关偏保守 | 单次瞬断的恢复窗口偏长 |

## 四、开发任务

### 任务 A1：请求内冷却升级抑制（修 R1，核心）

1. `RouteOutcome::RouteFailure` 增加字段 `repeat_within_request: bool`（或新枚举变体）。
   调用方（`finish_route_health_permit` 的各失败分支）依据
   `request_route_attempts` 判断：该 `RouteHealthKey` 在本请求内是否已记录过
   transient 族（`TransientServer`/`EdgeProxyError`/`CapacityUnavailable`）失败。
2. 健康注册表 `record_failure` 对 `repeat_within_request=true` 的瞬态失败：
   **只重置冷却起点、不升级 step**（`failure_step` 返回当前 step 而非 +1）。
   本地与 Redis 两个 backend（`route_health.rs` / `redis_runtime`）行为一致。
3. 效果：一次下游请求无论内部重试多少轮，对每条路由最多贡献 +1 step；
   跨请求的独立失败仍正常升级（保留真实故障的退避能力）。
4. 不引入新开关（该行为无害且始终正确）；在 tracing 的失败日志里带出
   `step_suppressed=true` 便于观测。

### 任务 A2：预算对齐的最后等待（修 R2）

1. `RouteRetryPolicy::decide` 在 `current_round >= max_rounds` 时不再无条件 None：
   若同时满足——
   - runtime 开关 `upstream_route_exhaustion_budget_alignment_enabled`（新增，默认 true）；
   - `health_recovery` 存在且 `class ∈ {TransientServer, EdgeProxyError}`；
   - 非 `client_retryable_rate_limit`（**保持 B3 不变量：429 族永远交还客户端**）；
   - `sleep_for = recovery.retry_after + jitter` ≤ 剩余预算；
   - 本请求尚未用过对齐等待（`RouteRetryBudget` 增加 `alignment_used: bool`）；
   —— 则返回一次标记为 alignment 的 `RouteRetryWait`，之后彻底放弃。
2. `log_route_retry_wait` 与 wait 结构带上 `alignment: bool`。
3. 语义总结：`max_rounds` 约束“盲重试”，时间预算约束“有依据的等待”；
   两者不再互相错位。

### 任务 A3：全冷却时的 last-resort 半开探测（修 R3/R4）

1. 触发条件：某一 routing round 结束时物理尝试数为 0，且所有被跳过的候选
   均因 transient 族冷却（ledger 的 cooled candidates 可判断）。
2. 行为：向健康注册表申请**提前半开**——新 API（如
   `reserve_route_health_probe`）：对“剩余冷却最短”的那条路由，忽略剩余冷却
   直接发放 half-open 单飞 lease（复用现有 half-open 单飞与
   `HalfOpenBusy` 机制），当前请求本身即探针：
   - 成功 → 走现有 half-open 成功路径清冷却，请求正常完成；
   - 失败 → 走现有 half-open 失败路径（step 受 `ROUTE_HALF_OPEN_FAILURE_STEP_CAP=5`
     封顶，且叠加 A1 的请求内抑制），随后按现有逻辑继续/终态。
3. 防雪崩约束：
   - 单飞：同一路由同时最多一个探测（现有 lease 机制天然保证）；
   - 最小间隔：同一路由两次提前探测 ≥1s（对齐 `HALF_OPEN_BUSY_RETRY`），
     注册表记录 `last_early_probe_at`；
   - 仅在“全冷却”时启用，只提前一条路由，不放开整池。
4. runtime 开关 `upstream_transient_last_resort_probe_enabled`（新增，默认 true）。
5. 效果：冷却窗口内到达的第一个请求变成探针；上游恢复的感知延迟从
   “冷却剩余时长”降到“下一个请求到达时刻”。配合 A2：探针失败才进入对齐等待。

### 任务 A4：参数与部署文档（修 R5）

1. `DEPLOYMENT.md`「Intranet / Aggregated Gateway Deployment」小节的设置表补三行：
   `upstream_transient_route_cooldown_base_seconds`（内网建议 2–3）、
   `upstream_transient_route_cooldown_max_seconds`（内网建议 60）、
   `upstream_route_exhaustion_retry_max_rounds`（配合 A2 保持 3 即可；未启用 A2 时建议 6），
   以及两个新开关的说明。
2. 排查指引补充：`upstream_routes_exhausted` 的 details 新字段（见 A5）如何读。

### 任务 A5：可观测性

1. `terminal_route_failure_error` 的 `details` 增加 `give_up_reason`
   （`round_cap` / `wait_budget` / `no_recovery` / `alignment_exhausted`）、
   `live_recovery_seconds`、`last_resort_probe_attempted: bool`。
   决策信息由 `decide`/循环侧透传。
2. `StreamDiagnostics`（`src/state/types.rs:733` 已有 `routing_rounds`）补
   `retry_waited_ms`、`give_up_reason`；postgres 往返测试同步扩展
   （`tests/postgres_roundtrip.rs` 既有 stream diagnostics 用例）。
3. `classify_dashboard_failure`（`src/server/admin.rs`）把
   `upstream_routes_exhausted` 终态单列（与 common-mode trip 并列），管理页可见趋势。

## 五、测试要求（Part A）

复用 `tests/gateway/chat/` 现有 mock 上游设施（`rate_limits.rs` 的
`route_retry_wait_budget_and_round_limit_are_bounded` 等既有用例为基线）：

1. **A1**：单请求 3 轮内同一路由失败 3 次 → 该路由 step 仅 +1（断言冷却时长不指数增长）；
   两个独立请求先后失败 → step 正常升级为 2。本地与 Redis backend 各一遍。
2. **A2**：轮数打满但 live recovery 在剩余预算内 → 多等一次并在恢复后成功；
   recovery 超出剩余预算 → 立即终态；`client_retryable_rate_limit` → 不对齐等待（B3 回归）；
   开关关闭 → 现状行为；对齐等待只发生一次。
3. **A3**：全部路由冷却期间到达的请求 → 恰好一条路由被提前半开并发出真实请求；
   mock 上游已恢复 → 请求成功且冷却清除；仍 502 → 终态错误、step 不超过半开封顶；
   1s 内的第二个请求不重复探测（单飞+间隔）；开关关闭 → 现状（0 物理尝试直接终态）。
4. **A5**：终态 details 含 `give_up_reason` 且与场景匹配；StreamDiagnostics 新字段
   postgres 往返；dashboard 分类单列。
5. 回归：2026-08-12 方案的 7 类用例全绿（共模熔断语义不变）。

---

# Part B：上下游模型名映射归一（去重复模型）

## 一、现状与问题（已核实代码）

上游各家对同一模型的命名大小写不一（`GLM-4.5` / `glm-4.5`、`DeepSeek-V3` /
`deepseek-v3`），而网关内所有模型匹配都是**大小写敏感的精确比较**：

1. 路由匹配 `canonical_route_model`（`src/state/normalize.rs:311-338`）：
   `candidate == model` 精确比较。下游请求 `glm-4.5` 无法命中声明为 `GLM-4.5`
   的上游 → 该上游被排除在候选之外（变相减少可用路由，加剧 Part A 的耗尽）。
2. 下游模型列表 `available_models_for_downstream`（`src/state.rs:4998-5018`）：
   `HashSet<String>` 精确去重 → `/v1/models` 同时出现 `GLM-4.5` 与 `glm-4.5`
   两个“模型”，客户端模型选择器重复。`downstream_visible_models`（`state.rs:5024`）、
   `list_models_codex_format`（`src/server/gateway.rs:2499`）同理。
3. 模型发现同步把上游 `/v1/models` 返回的名字**原样入库**到
   `supported_models` / `api_key_models`（`src/state/model_key_sync.rs:634-739`），
   不同上游的拼写差异因此直接进入配置。
4. 按 key 的模型匹配 `keys_for_model`（`normalize.rs:344-378`）、高端模型判断
   `is_premium_model_request`（`normalize.rs:199-211`）同为精确比较。
5. **既有先例**：使用量统计与 portal 允许清单已经做了小写归一
   （`normalize_model_name`，`src/state/usage.rs:224-230`；
   `portal_model_is_allowed` 双侧小写）。即：仓库内“同模型异拼写”的等价判断
   已有一半是大小写不敏感的，另一半（路由/列表/premium/key 映射）不是——行为不一致。

## 二、设计

### B1：canonical 归一层（默认行为，零配置解决大小写问题）

1. 新模块 `src/state/model_identity.rs`：
   - `pub fn canonical_model_id(model: &str) -> String`：`trim` + `to_ascii_lowercase`
     （与 `usage.rs` 先例一致）；
   - `pub fn models_equivalent(a: &str, b: &str) -> bool`；
   - 后续所有“以模型名做键或比较”的路径统一经由本模块，禁止散落的 `==`。
2. 改造点（全部改为 canonical 比较，**存储保持原拼写不动**）：
   - `canonical_route_model` / `supports_model` / `resolved_model_name`：
     大小写不敏感匹配；**返回值必须是该上游 `route_models()` 里存储的原拼写**
     （上游可能大小写敏感，发往上游的 model 字段与 `RouteHealthKey.runtime_model_slug`
     都必须用上游自己的拼写，`gateway.rs:5335` 起的消费方无需改动）；
   - `keys_for_model` / `api_key_models` 匹配；
   - `is_premium_model_request` / `premium_route_models` 判断；
   - affinity 键（`get_affinity_upstream` 以 model 为键的部分）、
     模型上下文配置查找（`ModelContextConfig.slug` 匹配）、模型允许清单（已小写，核对即可）；
   - `codex_subagent_base_model` 交互：先剥 subagent 后缀、再 canonical。
3. 下游模型列表去重：`available_models_for_downstream` / `downstream_visible_models` /
   `list_models_codex_format` 按 `canonical_model_id` 分组，每组只输出一个 id。
   无别名规则时的**显示拼写选择必须确定性**：默认输出 canonical（全小写）形式；
   显式别名规则（B2）可覆盖显示拼写。
4. 兜底开关：runtime setting `model_case_insensitive_matching`（默认 true）。
   置 false 完全回退到现状（应对某上游真的用大小写区分两个不同模型的极端情况）。

### B2：显式模型别名注册表（管理员配置，处理“超出大小写”的等价与重命名）

1. 持久化状态新增：
   ```rust
   pub struct ModelAliasRule {
       pub canonical: String,      // 下游展示与请求用的规范名（含期望的显示大小写）
       pub aliases: Vec<String>,   // 归入该规范名的其它拼写（大小写不敏感匹配）
   }
   // PersistedState 增加 model_aliases: Vec<ModelAliasRule>
   ```
   校验：alias 在全部规则内唯一（canonical 化后比较）；canonical 不得同时是
   其它规则的 alias；空串拒绝。
2. 解析顺序（请求路径与列表路径一致）：
   显式 alias 命中 → 该规则的 canonical；否则 → B1 大小写折叠。
   反向解析（选定某上游后）：在该上游 `route_models()` 中找出与请求模型
   等价（规则展开 + 大小写折叠）的**原拼写**作为 runtime_model_slug 发往上游。
3. 用途示例：
   - 归并：`{canonical: "glm-4.5", aliases: ["glm-4-5", "GLM-4.5-Preview"]}`；
   - 重命名/统一品牌名：`{canonical: "deepseek-v3", aliases: ["deepseek-chat"]}`；
   - 控制显示大小写：`{canonical: "GLM-4.5", aliases: []}`（组内展示用这个拼写）。
4. 管理面：
   - admin API：`GET/PUT /admin/model-aliases`（跟随现有 persisted state CRUD 与
     校验模式，如 upstream 配置）；
   - 管理前端：模型页新增“归并视图”——按 canonical 分组展示每个上游的实际拼写
     与来源（发现同步/手工），支持规则增删改；
   - 使用量/配额/premium/affinity 统计均记 canonical（usage 已是小写，做一次对齐核查）。
5. 发现同步（`model_key_sync`）**不改写存储拼写**（上游调用依赖原拼写），
   只在聚合与匹配层归一。

### 边界与风险

- 同一上游同时列出仅大小写不同的两个条目：视为同一模型，取首个拼写；
  极端场景用 `model_case_insensitive_matching=false` 或显式规则拆开。
- 下游继续用旧的大写拼写请求：canonical 化后照常命中（向后兼容）。
- 别名规则改动即时生效（走 routing snapshot 重建），无需重启。

## 三、测试要求（Part B）

1. 路由：上游声明 `GLM-4.5`，下游请求 `glm-4.5` → 命中该上游，且发往上游的
   payload model 与 `RouteHealthKey.runtime_model_slug` 为 `GLM-4.5`（原拼写）。
2. 列表：两个上游分别声明 `GLM-4.5` / `glm-4.5` → `/v1/models`（标准与 codex 两种
   格式）只出现一个条目；有显式规则时显示拼写取 canonical 字段。
3. 别名：`deepseek-chat → deepseek-v3` 规则下，请求 `deepseek-v3` 路由到只声明
   `deepseek-chat` 的上游且 payload 用 `deepseek-chat`；请求 `deepseek-chat`（旧名）
   同样可用。
4. key 映射与 premium：`api_key_models` / `premium_models` 拼写与请求大小写不同 →
   匹配成立；配额/用量落到 canonical。
5. 校验：alias 冲突（两规则含同一 alias、canonical 兼任 alias）→ 拒绝并给出明确错误。
6. 开关：`model_case_insensitive_matching=false` → 全部行为回退现状（回归用例）。
7. 持久化：`model_aliases` 经 JSON 与 postgres 往返不丢失（`tests/postgres_roundtrip.rs`）。
8. 回归：`codex_subagent_base_model` 相关既有用例全绿。

---

# 交付顺序建议

1. Part A 的 A1+A2（小改动、立竿见影，直接消除报错场景的主要痛点）；
2. Part A 的 A3（自愈能力质变）与 A5（观测）；
3. Part B 的 B1（默认归一）→ B2（别名注册表 + 管理面）；
4. 文档 A4 随各任务同步更新。

每个任务独立成 commit，测试先行（仓库惯例：`rtk cargo test` 全绿后提交）。
