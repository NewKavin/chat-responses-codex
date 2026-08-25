# 交付提示词 —— 修复上游 400 `extra data: line 1 column 3 (char 2)`

> 直接把下面整段发给实施模型。

---

## 你的任务

修复 chat2Responses 网关的一个**已定位、证据链完整**的缺陷。**根因已经闭环，不要重新排查、不要自行改变方向。你的任务是实施，不是诊断。**

完整方案见 `docs/superpowers/plans/2026-08-25-tool-call-arguments-concat-account-switch.md`，请先完整读一遍再动手。

## 现象

codex 侧间歇性上游 400：`message: extra data: line 1 column 3 (char 2)`。**每次都在上游账号切换之后出现**；一直使用同一个上游则不出现；出现后不会自愈，必须新建会话。模型为 GLM5.1 / deepseek-v4-flash-0731，经内网单台 new-api 聚合网关多 key 分流。

## 根因（已确认，逐跳可查）

1. Python `json.loads` 的 **`Extra data`** 分支只在**已消费完一个完整 JSON 值**后仍有剩余字节时抛出；`char 2` 说明该完整值长度恰为 2 字符。函数参数字段中长度为 2 的合法 JSON 只能是 **`{}`**。故污染串形状被唯一确定为 `{}` + 真实参数，例如 `{}{"command":["ls"]}`。

2. `src/protocol.rs:2508` 的 `fallback_index` 来自 `tool_calls.iter().enumerate()`，是**本次分片数组内的位置**，不是稳定的工具调用序号。

3. `src/protocol.rs:2833-2838` 合并键解析：缺 `index` 时，有 `id` 走 `call_id` 匹配（安全）；**`id` 也缺失则 `None => fallback_index`（不安全）**。

4. `src/protocol.rs:2851-2861` `entry(index).or_insert_with(...)` 键命中即复用已有条目，不校验 `call_id` / `name` 是否一致。

5. `src/protocol.rs:2881-2884`（以及 `:3015-3017` 的重复实现）`entry.arguments.push_str(&arguments)` **无条件追加**：不检测已有内容是否已是完整 JSON，无重置、无去重。

6. 于是：首片 `arguments:"{}"`（带 id，建条目 key=0）→ 续片缺 index 缺 id（取位置 0）→ 命中条目 0 → 追加 → `arguments = "{}{\"command\":[\"ls\"]}"`。

7. `emit_tool_call_done`（`src/protocol.rs:2756-2802`）把该串**原样**下发给 codex，并原样写入响应历史。

8. 下一轮 codex 带 `previous_response_id`，历史 items 被复原进 `input`（`src/server/gateway.rs:3840-3960`），请求方向转换 `src/protocol.rs:1579` / `:1765` / `tool_adapter.rs adapt_responses_function_call` **逐字节透传，无任何 JSON 校验** → 发到上游 → 上游 Python `json.loads` → 400。

9. 为什么与账号切换绑定：账号 A / B 是聚合网关上不同 channel，背后适配器不同，**工具调用续片是否携带 `index`/`id` 不一致**。A 带（累加器安全），B 不带（命中第 3 点的兜底分支）。切账号即切分片风格。为什么不自愈：污染串已持久化进历史，每轮原样重放。

**对照证据**：`src/protocol/tool_adapter.rs:800` `extract_custom_input` 对 `custom_tool_call` 的参数**有** `serde_json::from_str` 校验，而 `function_call` 分支没有。全仓 `serde_json::from_str(arguments)` 只出现在能力探测与诊断路径（`compatibility_semantics.rs` / `capability_probe.rs` / `troubleshooting.rs` / `claude.rs`），**热请求转换路径上一处都没有**。

## 已排除，不要再查

- 跨 attempt 状态残留：`src/server/gateway/stream.rs:1481` 每个 `UpstreamStreamReader` 新建 `StreamTranslator`。
- hedge 并发重复下发：`src/server/gateway/upstream.rs:1082-1116` 先选 winner 再下发，losers 一律 `cancel_as_loser()`。
- 流中途换路重放：`can_replay()` 全仓唯一生产调用点是 `upstream.rs:1158`（hedge 预取阶段）；`stream.rs:1005` / `:1780` 仅诊断用。
- 累积式分片被重复拼接：那会报 `Expecting …`，不会报 `Extra data`（见根因第 1 点）。
- 网关自身 serde_json：serde_json 报 `trailing characters`，`Extra data: line 1 column N (char M)` 是 Python 特有格式，抛错方在上游。

## 实施顺序

### 第 1 优先（止血，做完这两条 400 就应该消失）

**T1.2 累加完整性护栏** —— `src/protocol.rs:2881-2884` 与 `:3015-3017`，`push_str` 之前判定：若 `entry.arguments` 非空且**已能被 `serde_json::from_str::<Value>` 解析为完整值**，而新片段又以 `{` / `[` / `"` 开头 → 判定为「新值」而非「续写」，**禁止拼接**；记 `warn` + 计数 `reason="complete_then_new"`；取值策略**以新片段覆盖旧值**（因为观测到的实际形状是 `{}` 占位在前、真实参数在后），把该策略写成带注释的常量说明理由。此护栏不依赖上游分片风格，是决定性修复。

**T1.1 撤销按数组位置兜底的合并** —— `src/protocol.rs:2833-2839`。既无 `index` 又无 `id` 时**不得**使用 `fallback_index`，改为：
- 恰好只有**一个**未 `done_emitted` 的条目 → 合并到它；
- 有**多个**或**没有**未完成条目 → **不合并**，新建条目（键取 `max+1`），打 `warn` + 计数 `reason="ambiguous_continuation"`；
- 合成 `call_id` 不再用位置（现为 `format!("call-{}", index)`），改为不会与上游 id 冲突的稳定形式。

用运行时开关 `tool_call_merge_strict`（默认 **开**）控制，便于回退。

### 第 2 优先（阻断已存在污染的扩散）

- **T2.1** 新增统一函数 `normalize_tool_arguments(raw) -> (Cow<str>, Option<RepairReason>)`：空 → `{}`；完整合法 → 原样；**完整合法 + 尾随内容 → 若尾随部分本身也是完整 JSON 对象则取尾随那个**（对应 `{}`+真实参数），否则取第一个完整值，打 `TrailingData`；完全不可解析 → 默认 `warn` + 原样透传，`tool_arguments_strict` 开关打开时返回 400。实现建议用 `serde_json::Deserializer::from_str(raw).into_iter::<Value>()`。
- **T2.2** 三处请求方向转换全部接入同一函数，不允许各自实现：`src/protocol.rs:1579`、`src/protocol.rs:1765`、`src/protocol/tool_adapter.rs` 的 `adapt_responses_function_call`「function_call」分支。
- **T2.3** `emit_tool_call_done`（`src/protocol.rs:2756-2802`）在构造 `function_call_arguments.done` 与 `make_response_function_call_item` 之前也规范化一次。**你需要自行确认响应历史 items 的实际汇聚路径**（`src/protocol.rs:3053-3095` 的 output_items 汇聚 与 `src/state.rs:785` `store_response_history` 之间），确保写进历史的那一份也是规范化后的值——否则污染仍会持久化。
- **T2.4** 打点：`tool_arguments_repaired`、`tool_arguments_repair_reason`、`call_id`、`upstream_id`、`model`。

### 第 3 优先（跨账号历史复用，独立成立的次生缺陷）

`sanitize_history_for_cross_provider_replay` 定义在 `src/server/gateway.rs:3921`，**全仓只有一个调用点** `src/server/gateway.rs:7947`（continuation-pin escape 通道）。而响应历史键（`src/state.rs:785` / `:833`）是 `downstream_key_id` + `response_id`，**不含 upstream 维度** —— 普通账号轮换复用历史时**完全不清洗**。

- **T3.1** 历史条目增加 `source_profile`（`upstream_id` + key fingerprint + dialect profile key + protocol），`state.rs:785` 写入、`:833` 读取带出。
- **T3.2** `gateway.rs:3840-3960` 重放路径比较 `source_profile` 与本次实际 profile，**不一致即调用清洗**（不再只在 `:7947` 调用），打点 `history_cross_profile_replay=true`。保留 `:7947` 现有调用点不动。
- **T3.3** 跨 profile 重放时叠加 T2.1 规范化（历史里可能已有存量污染）。
- **T3.4 本轮不要做**：历史键加 profile 维度会让切账号后大量出现 `unknown previous_response_id` 400（见 `gateway.rs:3843`），留待观测数据支撑。

### 第 4 优先（验收依赖）

- **T4.1** 新增 `tool_call_arguments_anomaly` 事件/计数，维度 `reason`（`ambiguous_continuation` / `complete_then_new` / `trailing_data` / `unparseable`）、`upstream_id`、`model`、`request_id`。这是判断「哪个账号的分片风格异常」的唯一手段，也是验收指标。
- **T4.2** 上游 4xx message 摘要落日志，受既有 `upstream_error_body_excerpt_enabled` 控制。

### 可延后

- **T1.3** `ChatToResponsesState.tool_calls` 主键由 `usize` 改为 `call_id`（`index` 降级为次级键）。需同步 `src/protocol.rs:2844-2848`（`output_index` 复用）、`:2756-2802`（done 遍历，必须仍按 `output_index` 排序输出）、`:3095`（`sort_by_key`）。**独立提交**，T1.1+T1.2 已足够止血。
- **T1.4** 把 `:2881` 与 `:3015` 两处重复的累加逻辑抽成单一私有 helper（如 `merge_tool_call_arguments`），杜绝语义漂移。

## 必须写的三个根因守卫测试（当前代码必然失败）

1. 分片1 `{id:"call_a", name:"shell", arguments:"{}"}`，分片2 `{arguments:"{\"command\":[\"ls\"]}"}`（无 index 无 id）→ 断言最终 `arguments` 为 `{"command":["ls"]}`，**不含 `{}` 前缀**，且 `serde_json::from_str` 成功。
2. 已有两个未完成条目时来一个无 index 无 id 的续片 → 断言**不合并**进 index=0 的条目。
3. 历史 `source_profile` 与当前 profile 不一致的重放 → 断言 `sanitize_history_for_cross_provider_replay` 被调用。

## 必须保留的回归测试

- 续片缺 index 但**有** `id` 且与已有条目匹配 → 仍按 `call_id` 正确合并。
- 正常带 `index` 的纯增量分片（`{"comm` + `and":["ls"]}`）→ 仍正确拼接为 `{"command":["ls"]}`。**不要把正常增量拼接也一起禁掉。**
- 现有 hedge / dialect retry / continuation-pin 集成测试全绿。

## 集成测试

1. mock 上游按「账号 B 风格」分片（续片省略 `index` 与 `id`，首片 `arguments:"{}"`），走 `/v1/responses`，断言下发的 `response.function_call_arguments.done` 中 `arguments` 合法。
2. 第 1 轮走账号 A → 人为冷却 A → 第 2 轮带 `previous_response_id` 落到账号 B，断言**发往上游的请求体**中每个 `tool_calls[].function.arguments` 均可被 `serde_json::from_str` 解析。

## 约束

- **所有命令必须加 `rtk` 前缀，命令链里每一段都要加**（`rtk git add . && rtk git commit -m "..."`，不是 `git add . && git commit`）。
- 验证链：`rtk cargo fmt --all -- --check` → `rtk cargo clippy --all-targets --all-features -- -D warnings` → `rtk cargo test`。
- 每个行为变更都要有运行时开关（`tool_call_merge_strict` 默认开、`tool_arguments_strict` 默认关）。
- **T1.2 单独一个 commit**，便于单独回滚与验证。T1.3 也单独一个 commit。
- 不要动 `upstream_retry_after_cap_seconds` 等路由重试参数——那属于另一份文档 `2026-08-25-route-exhaustion-cooldown-budget-invariant.md` 的范围，是**独立缺陷**。
- 完成后回填计划文档 §6 的 commit hash 与 ✅。
