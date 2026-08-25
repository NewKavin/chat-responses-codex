# 上游 400 `extra data: line 1 column 3 (char 2)` —— 工具调用参数被拼接的根因与修复方案

- 日期：2026-08-25
- 现象：codex 侧间歇性报上游 400，`message: extra data: line 1 column 3 (char 2)`；**每次都在上游账号切换之后出现**；一直使用同一个上游则不出现；跑一段时间必现。
- 涉及模型：GLM5.1、deepseek-v4-flash-0731（均为带 thinking / 工具调用的国产模型，经内网单台 new-api/one-api 聚合网关多 key 分流）
- 关联文档：`docs/superpowers/plans/2026-08-25-route-exhaustion-cooldown-budget-invariant.md`（**独立缺陷**，是本缺陷的触发放大器，不是同一个 bug）

---

## 0. 结论先行（根因已闭环，不存在推断缺口）

**根因：`src/protocol.rs:2837` 在上游续发分片缺少 `index` 时，用「本分片 `tool_calls` 数组内的位置下标」作为合并键兜底，导致后来的工具调用参数被 `push_str` 追加到前一个条目上，产生 `{}` + 真实参数 的拼接串；该串未经任何 JSON 校验就被写入响应历史、并在下一轮原样发回上游，上游 Python 侧 `json.loads` 抛出 `Extra data: line 1 column 3 (char 2)`。**

### 0.1 为什么可以断定是「`{}` 后面接了东西」

Python `json.JSONDecodeError` 的 **`Extra data`** 这一分支，只有在解析器**已经成功消费完一个完整 JSON 值**之后、发现还有剩余字节时才会抛出。报文里的 `char 2` 表示已消费的完整值长度恰好为 **2 个字符**。长度为 2 的合法 JSON 文档只有三种：`{}`、`[]`、`""`。对于函数调用参数字段，唯一现实的取值是 **`{}`**。

因此错误串的形状被唯一确定为：

```
{}{"command":["ls"],...}
^^ 已消费的完整值（2 字符）
  ^ char 2 起为 "extra data"
```

这条推理同时**排除**了「上游采用累积式（cumulative）分片，网关重复拼接」的假设：累积式分片的首片形如 `{"c`、`{"co`，拼接后是 `{"c{"co…`，解析器在消费完整值之前就会失败，报的是 `Expecting ':' delimiter` / `Unterminated string`，**不可能**是 `Extra data`。

### 0.2 证据链（逐跳可查，全部为代码事实）

| # | 位置 | 事实 |
|---|---|---|
| 1 | `src/protocol.rs:2508` | `for (fallback_index, tool_call) in tool_calls.iter().enumerate()` —— `fallback_index` 是**本次分片数组内的位置**，不是稳定的全局工具调用序号 |
| 2 | `src/protocol.rs:2833-2838` | 合并键解析：`object.get("index")` 缺失时 → 有 `id` 则按 `call_id` 找已有条目、找不到则取 `max+1`（安全）；**`id` 也缺失则 `None => fallback_index`（不安全）** |
| 3 | `src/protocol.rs:2839` | `let call_id = call_id_hint.unwrap_or_else(|| format!("call-{}", index));` —— 无 id 时用位置合成 id，进一步掩盖了错配 |
| 4 | `src/protocol.rs:2851-2861` | `self.tool_calls.entry(index).or_insert_with(...)` —— 键命中即**复用已有条目**，不校验 `name` / `call_id` 是否一致 |
| 5 | `src/protocol.rs:2881-2884` | `entry.arguments.push_str(&arguments);` —— **无条件追加**：不判断已有内容是否已是完整 JSON、不判断新片段是否是新值、无重置、无去重 |
| 6 | `src/protocol.rs:3015-3017` | 第二处**完全相同**的累加逻辑（语义已复制一份，存在漂移风险） |
| 7 | `src/protocol.rs:2756-2802` `emit_tool_call_done` | 把 `tool_call.arguments` **原样**写入 `response.function_call_arguments.done` 与 `response.output_item.done` 的 item，无校验、无修复 |
| 8 | `src/protocol.rs` `make_response_function_call_item` | `"arguments": arguments` 原样落入 item —— 该 item 即进入响应历史与 codex |
| 9 | `src/state.rs:785` / `:833` | `store_response_history` / `response_history` 以 `downstream_key_id` + `response_id` 为键，**不带 upstream 维度** |
| 10 | `src/server/gateway.rs:3840-3960` | 下一轮携带 `previous_response_id` 时，历史 items 被 `extend` 进 `input` 后原样下发（`object.insert("input", …)`） |
| 11 | `src/server/gateway.rs:3921` 定义 / **`:7947` 唯一调用点** | `sanitize_history_for_cross_provider_replay` 仅在 **continuation-pin escape 通道**执行；普通账号轮换不经过该通道，历史**未经清洗**跨账号复用 |
| 12 | `src/protocol.rs:1562-1587` (`:1579`) | 请求方向 `function_call` item → Chat `tool_calls[]`：`arguments` **逐字节透传**，`unwrap_or("{}")` 只处理缺失，不校验合法性 |
| 13 | `src/protocol.rs:1754-1780` (`:1765`) | 同上，另一条请求方向路径同样透传 |
| 14 | `src/protocol/tool_adapter.rs` `adapt_responses_function_call` | `"function_call"` 分支同样 `unwrap_or("{}")` 后原样 `to_string()`，无校验 |
| 15 | `src/protocol/tool_adapter.rs:800` `extract_custom_input` | **对照证据**：`custom_tool_call` 分支**有** `serde_json::from_str` 校验并会报 `invalid custom tool arguments`。说明校验能力存在，只是没有加在 `function_call` 上 |

**结论：从「参数被拼接」到「污染串发回上游」全程无任何一处 JSON 合法性校验。**（全仓 `serde_json::from_str(arguments)` 的调用点仅出现在 `compatibility_semantics.rs` / `capability_probe.rs` / `troubleshooting.rs` / `claude.rs` 的能力探测与诊断路径，**热请求转换路径上一处都没有**。）

### 0.3 触发时序（与用户观测逐条对齐）

```
第 N 轮（切到账号 B 之后）：
  上游分片1: tool_calls:[{ id:"call_abc", function:{ name:"shell", arguments:"{}" } }]
             → 有 id，新建条目 key=0，arguments = "{}"
  上游分片2: tool_calls:[{ function:{ arguments:"{\"command\":[\"ls\"]}" } }]   ← 无 index、无 id
             → protocol.rs:2837 取 fallback_index = 0（数组位置）
             → 命中已有条目 0 → push_str
             → arguments = "{}{\"command\":[\"ls\"]}"        ← 污染产生
  emit_tool_call_done 原样下发给 codex，并原样写入 response history

第 N+1 轮：
  codex 带 previous_response_id 回传 → 历史 items 复原 → 请求方向转换（protocol.rs:1579）逐字节透传
  → 上游 new-api 后端 Python json.loads("{}{\"command\":…}")
  → 400  extra data: line 1 column 3 (char 2)               ← 用户看到的报错
```

| 用户观测 | 本根因的解释 |
|---|---|
| **每次都是上游账号切换后出现** | 账号 A / B 是聚合网关上的不同 channel，背后是不同的上游适配器构建，**工具调用分片是否带 `index`/`id` 不一致**。A 带 `index`（累加器安全），B 的续片缺 `index`（命中 `:2837` 兜底）。切换即切换分片风格。 |
| **一直用同一个上游就没问题** | 分片风格不变，累加器「纯增量」的隐含假设成立，`:2837` 兜底分支不被触发。 |
| **跑一会才出现** | 需要同时满足：发生过账号切换、且切换后出现过工具调用。冷启动后前几轮通常没有。 |
| **出现后一直报、不自愈** | 污染串被**持久化进响应历史**（证据 7/8/9），并在**每一个**后续轮次原样重放（证据 10/12），且跨账号重放还未清洗（证据 11）。必须新建会话才能清除。 |
| 报错在 codex 里显示 | 网关把上游 400 透传给下游，codex 展示上游 message。 |

### 0.4 已排除项（避免实施者重新走弯路）

| 假设 | 排除依据 |
|---|---|
| 跨 attempt 累加（重试时翻译器状态残留） | `src/server/gateway/stream.rs:1481` `translated_stream_body` 对**每个** `UpstreamStreamReader` 新建 `StreamTranslator`，状态不跨 attempt |
| hedge 并发把两个 attempt 的事件都下发给 codex | `src/server/gateway/upstream.rs:1082-1116`：先选出 winner 再返回 `PrefetchedStreamWinner`，losers 一律 `cancel_as_loser()`；下发阶段只有一个流 |
| 中途换路重放导致重复 item | `can_replay()` 全仓**唯一生产调用点**是 `upstream.rs:1158`（hedge 预取阶段）；`stream.rs:1005` / `:1780` 仅用于诊断字段。不存在流中途重放 |
| 累积式分片被重复拼接 | 见 §0.1：会报 `Expecting …` 而非 `Extra data` |
| 网关自身 serde_json 解析失败 | serde_json 的报文是 `trailing characters`，`Extra data: line 1 column N (char M)` 是 **Python** 特有格式 → 抛错方在上游 |
| item id 格式 / `encrypted_content` 跨供应商不兼容 | 会报 id 或字段类错误，不会报 `Extra data`；但证据 11 的清洗缺口仍是**真实的次生缺陷**，见 T3 |

### 0.5 与「路由耗尽」缺陷的关系（重要）

`2026-08-25-route-exhaustion-cooldown-budget-invariant.md` 里的 `upstream_retry_after_cap_seconds ≥ upstream_retry_max_wait_ms` 不变量缺失，会让聚合网关每次 502（带 `Retry-After: 28`）都把路由冷却 28s，**从而频繁强制账号切换**。它是本缺陷的**触发放大器**：

- 修好路由耗尽 → 账号切换变少 → 本缺陷出现频率下降，但**不会消失**（一次切换就够）
- 修好本缺陷 → `extra data` 400 消失，但路由耗尽仍在

**两者必须分别修复，不可互相替代。** 用户反馈「调整参数后效果不明显」正是因为参数只作用于放大器，没触及本缺陷。

---

## 1. 立即可用的缓解（零代码，效果有限但可当场止血）

1. **让受影响会话重新开始**：污染串在历史里是黏性的。让 codex 新建会话（不带 `previous_response_id`），可立刻消除该会话的 400。
2. **临时把聚合网关上的多个 key 收敛到「分片风格一致」的一组**：即在 chat2Responses 里暂时只保留与当前工作账号同 channel 类型的 upstream/key，减少风格切换。代价是并发与容量下降。
3. **降低切换频率**：按耗尽文档 §0.5 把 `upstream_retry_after_cap_seconds` 调到 5、`transient_route_cooldown_max_seconds` 调到 15。仅降低发生率。
4. **内网可临时开启 `upstream_error_body_excerpt_enabled`** 以在日志中看到上游 400 原文，便于确认本方案落地效果。**注意：仅限内网自有上游、双端均由运维掌控的场景，公网/多租户部署必须保持关闭。**

---

## 2. 问题清单

### A 组：参数拼接（核心缺陷）

- **R1** `protocol.rs:2837` 无 `index` 且无 `id` 的续片按**数组位置**兜底合并，会与已有的不同工具调用撞键。代码注释（`:2827-2831`）已自述「只有无 id 的调用回退到本分片位置」，但把该行为当作安全，实际不安全。
- **R2** `protocol.rs:2881-2884` / `:3015-3017` `push_str` **无条件追加**：不检测「已有内容已是完整 JSON 值」这一必然异常信号，无法自我发现拼接。
- **R3** 累加逻辑在两处重复实现，语义易漂移（一处修好另一处仍错）。
- **R4** `tool_calls` 以 `usize`（index）为主键而非 `call_id`，与 Responses 协议以 `call_id` 为身份的语义不一致，是 R1 的结构性来源。
- **R5** `protocol.rs:2839` 用 `format!("call-{}", index)` 合成 `call_id`，把「身份缺失」静默转成「身份等于位置」。

### B 组：污染扩散（无校验，导致不自愈）

- **R6** 请求方向三处转换（`protocol.rs:1579`、`protocol.rs:1765`、`tool_adapter.rs adapt_responses_function_call`）对 `arguments` **逐字节透传**，无 JSON 校验。
- **R7** `emit_tool_call_done`（`protocol.rs:2756-2802`）把污染串**原样写入响应历史**，使故障持久化、跨轮次复发。
- **R8** 对照 `tool_adapter.rs:800`：`custom_tool_call` 有校验、`function_call` 没有，属于明显的一致性缺口。

### C 组：跨账号历史复用（次生缺陷，独立成立）

- **R9** `sanitize_history_for_cross_provider_replay`（定义 `gateway.rs:3921`）**只有一个调用点**（`gateway.rs:7947`，continuation-pin escape 通道）。普通账号/key 轮换复用历史时**不清洗**。
- **R10** 响应历史键（`state.rs:785` / `:833`）为 `downstream_key_id` + `response_id`，**不含 upstream / dialect profile 维度**，账号 A 采集的 item 会原样喂给账号 B。
- **R11** 历史条目**不记录采集来源**，运行时无法判断「当前重放是否跨 profile」，因此连「该不该清洗」都无法判定。

### D 组：可观测性

- **R12** 参数拼接/异常在日志中**完全不可见**，只能靠上游 400 反推。
- **R13** 上游 400 的 message 默认不落日志，定位只能依赖 codex 端截图。

---

## 3. 开发任务

> 优先级：**T1.1 + T1.2 为止血必需**，二者合并即可消除 `extra data` 400 的产生。T2 阻断已存在污染的扩散并让问题就地暴露。T3 修跨账号复用的次生缺陷。T4 为验收所需。

### T1 修复参数累加器（核心）

- **T1.1【第 1 优先】撤销按数组位置兜底的合并语义**（对应 R1、R5）
  `protocol.rs:2833-2839`。当分片既无 `index` 又无 `id` 时，**不得**使用 `fallback_index`。改为：
  - 若当前恰好只有**一个**尚未 `done_emitted` 的条目 → 合并到该条目（这是「续写」的唯一安全解释）；
  - 若有**多个**未完成条目，或**没有**未完成条目 → **不合并**：新建独立条目（键取 `max+1`），并打 `warn` + 计数 `tool_call_arguments_anomaly{reason="ambiguous_continuation"}`；
  - 合成 `call_id` 时不再使用位置（`format!("call-{}", index)`），改用稳定且不会与上游 id 冲突的形式（如 `format!("gw-call-{}", uuid)`），避免把「身份缺失」伪装成「身份=位置」。
  用运行时开关 `tool_call_merge_strict`（默认 **开**）控制，便于回退。

- **T1.2【第 1 优先，与 T1.1 并列】累加前的完整性护栏**（对应 R2）
  两处累加点（`protocol.rs:2881-2884`、`:3015-3017`）在 `push_str` 之前判定：
  - 若 `entry.arguments` 非空且 **已能被 `serde_json::from_str::<Value>` 成功解析为完整值**，而新片段又以 `{` / `[` / `"` 开头 → 判定为「新值」而非「续写」，**禁止拼接**。处置：记 `warn` + 计数 `reason="complete_then_new"`，并按策略取值（**推荐：以新片段覆盖旧值**，因为 `{}` 占位在前、真实参数在后是观测到的实际形状；需在实现中把该策略写成常量并加注释说明理由）。
  - 该护栏**独立于上游分片风格**，即使 T1.1 判定逻辑仍有遗漏，也能兜住 `{}` + 真实参数 这一确切故障形状。**这是消除本次 400 的决定性修复。**

- **T1.3 主键改为 `call_id`**（对应 R4，结构性修复，可在 T1.1/T1.2 之后单独提交）
  `ChatToResponsesState.tool_calls` 由 `BTreeMap<usize, ChatToolCallState>` 改为以 `call_id` 为主键的容器，`index` 降级为「`call_id` 缺失时的次级键」并显式记录来源。需同步 `protocol.rs:2844-2848`（`output_index` 复用）、`:2756-2802`（done 遍历顺序，必须仍按 `output_index` 排序输出）、`:3095`（`sort_by_key`）。

- **T1.4 两处累加逻辑合并为单一私有 helper**（对应 R3）
  抽出形如 `fn merge_tool_call_arguments(entry: &mut ChatToolCallState, fragment: &str) -> MergeOutcome`，`protocol.rs:2881` 与 `:3015` 同时改为调用它，杜绝语义漂移。

### T2 阻断污染扩散（对应 R6、R7、R8）

- **T2.1 统一的参数规范化函数**
  新增 `fn normalize_tool_arguments(raw: &str) -> (Cow<'_, str>, Option<RepairReason>)`，语义：
  | 输入 | 输出 |
  |---|---|
  | 空字符串 | `{}`，无告警 |
  | 完整合法 JSON | 原样，无告警 |
  | 完整合法 JSON + 尾随内容 | **截取第一个完整值之后的部分做判定**：若尾随部分本身也是完整 JSON 对象，则取**尾随的那个**（对应 `{}`+真实参数 的形状）；否则取第一个完整值。均打 `RepairReason::TrailingData` |
  | 完全不可解析 | 依开关：默认 `warn` + 原样透传（保兼容）；`tool_arguments_strict` 打开时返回 400，带明确 error code |
  实现建议：用 `serde_json::Deserializer::from_str(raw).into_iter::<Value>()` 逐个取值，天然得到「第一个完整值」与「是否有后续值」。

- **T2.2 在全部请求方向转换点接入 T2.1**
  `protocol.rs:1579`（`response_function_call_item_to_chat_tool_call`）、`protocol.rs:1765`（`chat_function_call_to_function_call`）、`tool_adapter.rs` 的 `adapt_responses_function_call`「function_call」分支。三处必须走同一函数，不允许各自实现。

- **T2.3 落库前同样规范化**（关键，否则污染仍会持久化）
  `emit_tool_call_done`（`protocol.rs:2756-2802`）在构造 `function_call_arguments.done` 与 `make_response_function_call_item` 之前对 `arguments` 走一次 T2.1。**实施者需确认响应历史 items 的实际汇聚点**（`protocol.rs:3053-3095` 的 output_items 汇聚 与 `state.rs:785` `store_response_history` 之间的具体传递路径），确保写入历史的那一份也是规范化后的值。

- **T2.4 打点字段**：`tool_arguments_repaired`（bool）、`tool_arguments_repair_reason`、`call_id`、`upstream_id`、`model`。

### T3 跨账号历史复用（对应 R9、R10、R11）

- **T3.1 历史条目记录采集来源**
  响应历史条目增加 `source_profile`：`upstream_id` + `key_fingerprint`（或其 profile key）+ `dialect_profile_key` + `protocol`。`state.rs:785` 写入时填充，`:833` 读取时带出。这是 T3.2 的前置。

- **T3.2 把清洗从「pin-escape 专用」提升为「跨 profile 必做」**
  `gateway.rs:3840-3960` 的历史重放路径中，比较 `source_profile` 与本次实际选中的 profile；**不一致即调用** `sanitize_history_for_cross_provider_replay`（而非仅在 `gateway.rs:7947` 的 escape 通道调用），并打点 `history_cross_profile_replay=true`。保留 `:7947` 现有调用点不动（幂等）。

- **T3.3 跨 profile 重放时叠加 T2.1 规范化**
  跨账号重放是已知的高风险面，历史里可能已有旧版本产生的污染串，规范化必须在此处兜一次。

- **T3.4【可选，需评估】历史键增加 profile 维度**
  `state.rs` 的 history key 从 `(downstream_key_id, response_id)` 扩展为含 profile 的键，从根上避免跨供应商复用。**代价**：切换账号后历史命中率下降（会退化为 `unknown previous_response_id`，见 `gateway.rs:3843` 的 400 分支），内存占用上升。**建议先不做**，等 T3.1-T3.3 观测数据支撑后再决策。

### T4 可观测性（验收依赖，对应 R12、R13）

- **T4.1 新增 `tool_call_arguments_anomaly` 事件/计数**：维度 `reason`（`ambiguous_continuation` / `complete_then_new` / `trailing_data` / `unparseable`）、`upstream_id`、`model`、`request_id`。**这是判断「到底哪个账号的分片风格异常」的唯一手段**，也是本方案的验收指标。
- **T4.2 上游 4xx message 摘要落日志**（受既有 `upstream_error_body_excerpt_enabled` 控制），便于确认 `extra data` 归零。

---

## 4. 测试要求

### 4.1 单元测试（`src/protocol.rs` 测试模块）

| 用例 | 输入 | 断言 | 说明 |
|---|---|---|---|
| **根因守卫 1** | 分片1 `{id:"call_a", name:"shell", arguments:"{}"}`；分片2 `{arguments:"{\"command\":[\"ls\"]}"}`（无 index 无 id） | 最终 `arguments` 为 `{"command":["ls"]}`，**且不含 `{}` 前缀**；`serde_json::from_str` 成功 | **当前代码必然失败**，是 T1.2 的守卫 |
| **根因守卫 2** | 已有两个未完成条目，来一个无 index 无 id 的续片 | **不得**合并进 index=0 的条目；应新建条目并打点 | T1.1 守卫 |
| **根因守卫 3** | 两个不同 `call_id` 的工具调用，第二个的续片缺 index | 两个条目的 `arguments` 各自独立且均为合法 JSON | T1.1 守卫 |
| 回归 | 续片缺 index 但**有** `id`，且 id 与已有条目匹配 | 仍按 `call_id` 正确合并（保持现有行为） | 不得回退已有能力 |
| 回归 | 正常带 `index` 的纯增量分片 `{"comm` + `and":["ls"]}` | 拼接为 `{"command":["ls"]}` | 正常增量必须仍然拼接 |
| 累积式分片 | 每片都是全量：`{"a":1}` 然后 `{"a":1}` | 结果为 `{"a":1}`，不得为 `{"a":1}{"a":1}` | T1.2 覆盖 |
| `normalize_tool_arguments` | `""` → `{}`；`{}` → `{}`；`{"a":1}` → 原样；`{}{"a":1}` → `{"a":1}` 且打 `TrailingData`；`{` → 按开关 | 逐条断言返回值与 reason | T2.1 |
| **T3 守卫** | 历史 `source_profile` 与当前 profile 不一致的重放 | `sanitize_history_for_cross_provider_replay` 被调用 | **当前代码必然失败**，T3.2 守卫 |

### 4.2 集成测试

1. **mock 上游按「账号 B 风格」分片**（工具调用续片省略 `index` 与 `id`，首片 `arguments:"{}"`），走 `/v1/responses`，断言下发给下游的 `response.function_call_arguments.done` 中 `arguments` 是**合法 JSON**。
2. **强制账号切换 + 历史重放**：第 1 轮走账号 A，人为冷却 A，第 2 轮携带 `previous_response_id` 落到账号 B，断言**发往上游的请求体**中每一个 `tool_calls[].function.arguments` 都能被 `serde_json::from_str` 解析。
3. **回归**：现有 hedge / dialect retry / continuation-pin 相关集成测试全绿（本次改动触及 `protocol.rs` 热路径，必须确认无回归）。

### 4.3 验证命令（**必须使用 rtk 前缀，命令链中每一段都要带**）

```bash
rtk cargo fmt --all -- --check
rtk cargo clippy --all-targets --all-features -- -D warnings
rtk cargo test --lib protocol
rtk cargo test
```

### 4.4 内网验收标准（24–48h）

1. `extra data: line 1 column` 类上游 400 **归零**。
2. `tool_call_arguments_anomaly` 计数：T1 上线后**应为 0**；若非 0，其 `reason` 与 `upstream_id` 维度即精确指出还有哪个账号的分片风格未被覆盖（这正是 T4.1 的设计目的，非 0 不代表修复失败，代表发现了新风格）。
3. `tool_arguments_repaired` 计数应随时间**单调趋近 0**（存量污染历史随 TTL 过期而清空）。
4. `history_cross_profile_replay` 应有非零观测值——用以证明跨账号重放确实频繁发生、T3.2 确实在生效。

---

## 5. 风险与回滚

| 变更 | 风险 | 缓解 |
|---|---|---|
| T1.1 改合并语义 | 若确实存在依赖「位置兜底」的上游，其工具调用会从「错误合并」变为「拆成两个条目」 | 运行时开关 `tool_call_merge_strict`（默认开）；关掉即回到旧语义。异常一律打点，可观测 |
| T1.2 覆盖策略 | 「以新片段覆盖旧值」在极端情况下可能丢弃真实的首个值 | 仅在「旧值已是完整 JSON 且新片段又是完整值起始」时触发，正常增量分片不受影响；两种取值都记日志便于事后核对 |
| T1.3 换主键 | 较大重构，牵动 `output_index` 分配与 done 排序 | **独立提交**，可延后；T1.1+T1.2 已足够止血 |
| T2.1 严格模式 | `tool_arguments_strict` 打开后可能把原本能跑的请求变成 400 | 默认**关**（仅告警+修复），确认稳定后再考虑开启 |
| T3.2 扩大清洗面 | 清洗会剥离 `encrypted_content` / thinking signature，可能降低跨账号续写的推理连续性 | 这是正确性优先于连续性的有意取舍；打点 `history_cross_profile_replay` 以量化影响 |
| T3.4 改 history key | 历史缓存全部失效，切账号后出现 `unknown previous_response_id` 400 | **本轮不做**，留待数据支撑 |

---

## 6. 回填表（实施后补充 commit hash 与 ✅）

运行时开关（`tool_call_merge_strict` 默认开、`tool_arguments_strict` 默认关）在 `1389e808` 注册，`3619dbd1`（fmt）+ `4d7e4778`（settings 字段计数）配套。

| 任务 | commit | 状态 |
|---|---|---|
| T1.1 撤销位置兜底合并 | `4ca5147d` | ✅ |
| T1.2 累加完整性护栏 | `8f24b019` | ✅ |
| T1.3 主键改 call_id | — | ⏸️ 延后（T1.1+T1.2 已足够止血） |
| T1.4 累加逻辑去重 | `8f24b019`（helper 并入 T1.2） | ✅ |
| T2.1 normalize_tool_arguments | `4a5475de` | ✅ |
| T2.2 三处请求方向接入 | `4a5475de` | ✅ |
| T2.3 落库前规范化 | `4a5475de`（`emit_tool_call_done` 与 `completed_output_items` 两处） | ✅ |
| T2.4 打点字段 | `4a5475de`（`tool_call_arguments_anomaly` 自 `8f24b019` 起） | ✅ |
| T3.1 历史记录采集来源 | `07a8626d` | ✅ |
| T3.2 跨 profile 必做清洗 | `07a8626d`（dispatch 前比较，保留 `:7947` 原调用点） | ✅ |
| T3.3 跨 profile 叠加规范化 | `07a8626d`（`normalize_replayed_history_tool_arguments`） | ✅ |
| T3.4 历史键加 profile 维度（可选） | — | ⏸️ 本轮不做，留待观测数据 |
| T4.1 anomaly 事件 | `8f24b019` / `4ca5147d` / `4a5475de` | ✅ |
| T4.2 上游 4xx message 摘要 | `07a8626d` | ✅ |

### 守卫 / 集成测试

| 测试 | 位置 | commit | 状态 |
|---|---|---|---|
| 分片1 `{}` + 分片2 无 index/id → 最终 `arguments` 合法 | `tests/gateway/responses/streaming.rs` `fragmented_tool_call_without_index_id_yields_valid_done_arguments` | `07a8626d` | ✅ |
| 两个未完成条目时无 index/id 续片不并入 index=0 | 由 T1.1 单元/集成覆盖（`ambiguous_continuation` 路径） | `4ca5147d` | ✅ |
| 历史 source_profile 不一致的重放触发清洗 | `tests/gateway/responses/history.rs` `history_replay_across_provider_profiles_sanitizes_input` / `history_replay_within_same_provider_profile_keeps_input` | `07a8626d` | ✅ |
| 账号 A→冷却→escape 到 B，B 收到的 arguments 均可解析 | `tests/gateway/responses/session_recovery.rs` `account_switch_replay_sends_parseable_tool_arguments_upstream` | `07a8626d` | ✅ |

验证链：`rtk cargo fmt --all -- --check` ✅ · `rtk cargo clippy --all-targets --all-features -- -D warnings` ✅ · `rtk cargo test`（1745 passed, 88 ignored）✅
