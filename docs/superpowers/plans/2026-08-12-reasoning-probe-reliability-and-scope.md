# 方案：一键探测思考档位——内网失败根因修复 + 严格按所选模型探测

日期：2026-08-12
状态：待开发（本文档为实施方案，交给开发模型执行）
涉及端：Rust 后端（`src/server/gateway/capability_probe.rs`、`src/server/gateway/capability_admin.rs`、`src/state.rs`）、前端（`frontend/src/views/admin/ModelProbe.vue`、`frontend/src/api/admin.ts`、`frontend/src/utils/capabilityDiscovery.ts`）

---

## 一、问题现象

1. 部署到内网后，点击「一键探测思考档位」，没有任何模型能探测出档位（包括 `deepseek-v4-flash`，其内置策略候选齐全，手动 curl 上游带 `reasoning_effort` 是能拿到思考输出的）。
2. 在探测模型下拉框中选择了部分模型，但探测看起来仍是全量上游模型探测。用户要求：**只探测明确选择的模型，绝不隐式全量探测**。

## 二、根因分析（已核实代码）

### 根因 A（主因）：单 case 超时 20 秒 + 超时即丢弃全部证据

- 每个探测 case 被 `tokio::time::timeout(self.request_timeout, ...)` 包裹，`request_timeout` 来自运行时设置 `capability_probe_request_timeout_seconds`，环境变量默认 **20 秒**（`src/main.rs:138-142`）。
- `ReasoningControl` case 发送**非流式**请求且**不带任何输出 token 上限**（`capability_probe.rs:1796-1841`，body 只有 model+messages+档位字段）。思考模型在 medium 以上档位非流式响应经常远超 20 秒。
- 超时后 `run_plan` **立即返回 `OperationalFailure{code:"probe_timeout"}` 并丢弃之前所有已收集的证据**（`capability_probe.rs:876-885`）——哪怕 low/medium 已探测成功，整个 profile 也只记一次运营失败，档位为空，且 `next_probe_at` 进入指数退避（`profile.rs:106-109`），导致后续再点按钮时该路由显示 deferred/冷却。
- 加重项：`probe_plan_for_route` 里 `reasoning_trigger` 取候选档位的 **`values.last()`**（`capability_probe.rs:223-232`），对 deepseek/glm 就是 `"max"`。带 max 档位的 ToolContinuation case 在同一个 20 秒预算内要完成**两次串联请求**，对思考模型几乎必然超时。而该 case 排在 ReasoningControl 之前，直接让整个计划在到达档位探测前就中止。

### 根因 B：任意一个 case 出现 429/5xx/网络错误即中止整个计划

- `run_plan` 中 verdict 为 `Unobserved` 且 http_status ∈ {401,403,429,5xx,None} 时，整个计划立刻以 OperationalFailure 结束（`capability_probe.rs:887-900`）。
- 一键探测默认并发 4（`CAPABILITY_PROBE_CONCURRENCY`），每条路由要串行打 13+ 个真实请求（含图片、工具、流式等与档位无关的 case）。内网经 one-api/new-api 类聚合网关很容易触发 RPM 429，一个 429 就让该路由全军覆没。手动单发探测不会触发限流，所以「手动可以、按钮不行」。

### 根因 C：非流式响应缺少思考证据 → 200 也判为 rejected

- `ReasoningControl` 的 Supported 判定要求**非流式响应体**携带思考证据（`reasoning_response_has_evidence`，`capability_probe.rs:2221-2255`）：非空 `reasoning_content` / `message.reasoning` / `usage.*.reasoning_tokens>0` / Responses `reasoning` output item。
- 纯 200 无证据 → `reasoning_control_ignored`（Rejected，`capability_probe.rs:1820-1834`）。部分内网聚合网关只在流式模式透传 `reasoning_content`，非流式聚合时丢弃该字段——用户手动验证多为流式，所以感觉「上游明明可以」。

### 根因 D（“选择不生效”的三个可能来源，需逐一处理）

1. **部署版本滞后**：模型范围过滤（`state.rs:1947`）、批次状态接口是最近提交（`738dfc3`、`dd56bf5`、`cbefdb9`）才加入的。内网部署若早于这些提交，后端 `ProbeAllRequest` 收到 `models` 字段（`deny_unknown_fields`）会直接 400 或被忽略。需先确认内网构建包含这些提交。
2. **UI 口径误导**：「思考档位」tab 下方的「模型汇总/精确路由」表展示的是**全局** discovery（所有路由），不是本批次范围。用户点完按钮看到全部模型的路由都在列表里，容易误判为全量探测。
3. **后台自动全量探测**：`CapabilityProbeService` 的 1 秒 reconcile tick（`capability_probe.rs:507-515` → `state.rs:4850 reconcile_dialect_profiles`）会给**所有下游可见且 profile 缺失/过期**的路由自动排队探测，只要运行时设置 `automatic_capability_probes_enabled` 为 true（默认 false，但管理页可开）。这是真正意义上的全量探测来源，与按钮选择无关。
4. **前端兜底逻辑**：`ModelProbe.vue` 的 `loadProbeModelScope` 失败时保持空集合，注释写明「空集合 = 后端全量探测」。这个隐式兜底违背用户要求，必须移除。

---

## 三、开发任务

### 任务 1：探测计划的档位专用模式（后端，优先级最高）

目标：按钮只为发现思考档位服务，不再跑完整能力计划。

1. `ProbeAllRequest`（`capability_admin.rs:36-41`）新增字段 `mode: Option<String>`，取值 `"reasoning"` | `"full"`，默认 `"full"` 保持兼容。
2. `ProbeJob` / `queue_manual_capability_probe_batch` 传递 mode；`probe_plan_for_job`（`capability_probe.rs:295`）在 reasoning 模式下生成精简计划：
   - `MinimalText { stream: false }`（连通性 gate，1 个请求）；
   - 全部 `ReasoningControl` 候选 case；
   - 不包含 tools/图片/流式/usage/TokenLimit/Declarative case。
3. reasoning 模式的探测结果必须走 `apply_probe_outcome_partial`（合并，不整体替换 profile），避免精简计划把已验证的其他能力清空。`run_probe_job`（`capability_probe.rs:674`）里按 mode 选择 apply 路径。
4. 前端「一键探测思考档位」按钮请求带 `mode: "reasoning"`；「真实验证并应用」维持 full。

### 任务 2：case 级失败隔离，杜绝"一个失败全盘丢弃"（后端）

修改 `ProbeExecutor::run_plan`（`capability_probe.rs:851-908`）：

1. **超时不再中止计划**：单 case 超时记为该 case 的 `Unobserved { operational_code: "probe_case_timeout" }`，继续执行后续 case。整个计划只有在**首个 case（连通性 gate）**就 401/403 失败时才整体判 OperationalFailure。
2. **429/5xx 降级为跳过而非中止**：verdict 为 Unobserved 且状态 429/5xx 时，记录该 case 后继续；仅 401/403（凭据错误，重试无意义）保留立即中止。429 时读取 `Retry-After`，在不超过剩余预算的前提下 sleep 后重试一次。
3. 新增**计划级总预算**（如 `case 数 × 单 case 超时`，上限 10 分钟），防止逐 case 重试导致无限拖长。
4. 结束时若存在至少一个 Supported/Rejected 证据 → 走 Conclusive（若有 case 被跳过则用 `apply_probe_outcome_partial` 合并）；全部 Unobserved 才记 OperationalFailure。
5. 保留证据码：跳过的 case 以 operational_code 写入 `evidence_codes`，便于 UI 诊断列展示。

### 任务 3：档位探测改流式 + 早停（后端，解决根因 C 与超时）

改造 `CoreProbeCase::ReasoningControl` 执行（`capability_probe.rs:1796-1841`）：

1. 先发**流式**请求（chat: `stream:true` + `stream_options.include_usage:true`；responses: `stream:true`）。
2. 在流式聚合回调中观察思考证据：chat 的 `choices[].delta.reasoning_content` / GLM `delta.reasoning`、responses 的 `response.reasoning_summary_text.delta` / `response.output_item.added(type=reasoning)`、最终 chunk 的 `usage.*.reasoning_tokens>0`。
3. **一旦观察到首个思考增量，立即判 Supported 并主动断开连接（drop response stream）**——不等模型思考完。这把单档位探测耗时从"整个思考时长"压缩到"首 token 时延"，从根本上化解 20 秒超时问题。
4. 若上游拒绝流式（400），回退一次非流式请求，沿用现有 `reasoning_response_has_evidence` 判定。
5. 现有非流式判定逻辑保留为回退路径，`reasoning_control_ignored` 语义不变。
6. `reasoning_trigger`（`capability_probe.rs:223-232`）从 `values.last()` 改为 `values.first()`（low），避免 full 模式下 max 档位拖垮 ToolContinuation。

### 任务 4：超时与并发的运行时参数调整（后端 + 文档)

1. 新增运行时设置 `capability_probe_reasoning_timeout_seconds`（默认 90，环境变量 `CAPABILITY_PROBE_REASONING_TIMEOUT_SECONDS`），用于 `ReasoningControl` 与带 reasoning_trigger 的 case；其余 case 沿用现有 20 秒。
2. reasoning 模式批次内，**同一上游并发压到 1**（`ProbeQueueState::set_limits` 已支持 per-upstream 并发，直接在 reasoning 模式下传 1），case 间加 300–500ms 间隔，避免触发内网网关 RPM。
3. `docs/` 部署文档补充这两个环境变量说明。

### 任务 5：严格按所选模型探测（前后端，用户硬性要求）

1. **后端**：`admin_capability_probe_all`（`capability_admin.rs:462`）当请求携带 `mode:"reasoning"` 时，`models` 为空直接返回 400 `capability_probe_scope_required`（"必须显式选择探测模型"）。旧 full 模式保持空=全量的兼容语义（供"真实验证并应用"使用）。
2. **后端**：受理时以 info 级日志输出生效范围：`batch_id`、`models`、每条 candidate 的 upstream/route/protocol，便于内网核对。
3. **前端** `ModelProbe.vue`：
   - `loadProbeModelScope` 不再默认全选（删除 `selectedProbeModels.value = [...models]`），改为空选；
   - 删除「范围加载失败→空集合→后端全量」的兜底注释与行为；加载失败时禁用探测按钮并提示重试；
   - 按钮在 `selectedProbeModels.length === 0` 时 disabled，tooltip 提示"请先选择要探测的模型"；
   - 结果区新增"仅显示本批次模型"过滤（默认开启），全局 discovery 列表放到折叠区，消除"看起来全量"的误导。
4. **自动探测**：确认 `automatic_capability_probes_enabled` 默认 false；管理设置页该开关旁加说明文案"开启后会周期性对所有下游可见模型自动探测（消耗 token）"。reconcile 自动探测（`state.rs:4850`）保持受该开关控制，不做其他改动。
5. **部署核对项**（交付时在 PR 描述注明）：内网构建必须包含 `738dfc3`、`dd56bf5`、`cbefdb9`、`0e57dc2` 之后的代码，否则 models 过滤与批次状态接口不存在。

### 任务 6：诊断可见性（前端，小改动）

1. 「精确路由」表的诊断列已显示 `operational_code`；补充对 `probe_timeout` / `probe_case_timeout` / `probe_stream_transport_failed` / `reasoning_control_ignored` 的中文释义映射（tooltip），让内网用户能自助定位：
   - `probe_timeout`/`probe_case_timeout` → "上游响应超过探测超时，请调大 CAPABILITY_PROBE_REASONING_TIMEOUT_SECONDS"；
   - `reasoning_control_ignored` → "上游返回 200 但响应中无思考内容，档位可能被网关忽略"。
2. 批次候选表（本轮探测状态）诊断列同样接入该映射。

---

## 四、测试要求

复用 `tests/capability_probe.rs` 既有 mock 设施（`chat_probe_stream_mock`、`run_probe_against` 等）：

1. reasoning 模式计划只含 MinimalText(nonstream) + ReasoningControl case（断言 case 列表）。
2. 单 case 超时后计划继续执行、已获档位保留（partial 合并）；全部 case 超时才记 OperationalFailure。
3. 中途一个 429：该 case 跳过（带 Retry-After 时重试一次），后续档位 case 仍执行。
4. 流式档位探测：mock 首个 `reasoning_content` delta 后即判 Supported 且不再读流；流式 400 回退非流式。
5. `mode:"reasoning"` + 空 models → 400 `capability_probe_scope_required`；带 2 个模型 → 批次 candidates 仅覆盖这 2 个模型对应路由（断言其余模型不在 candidates）。
6. 前端 vitest：按钮空选禁用；结果默认按批次模型过滤。

## 五、内网立即可用的临时缓解（不改代码）

- 环境变量 `CAPABILITY_PROBE_REQUEST_TIMEOUT_SECONDS=180`、`CAPABILITY_PROBE_CONCURRENCY=1` 后重启，再点一次按钮——可绕过根因 A/B 的大部分场景，先验证 deepseek-v4-flash 能否出档位。
- 若诊断列显示 `reasoning_control_ignored`，说明命中根因 C（网关非流式不回思考内容），必须等任务 3 上线。
- 确认管理设置里 `automatic_capability_probes_enabled` 处于关闭状态，排除后台自动全量探测。
