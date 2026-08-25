# 交付提示词：工具调用身份判定与 anomaly 维度补齐

## 背景（不要重新排查）

`chat2Responses` 上一轮已经修掉了上游 400 `extra data: line 1 column 3 (char 2)` 的根因（工具调用参数被拼接），提交范围 `1389e808` → `49246447`，代码核查通过、clippy 干净、`rtk cargo test` 1745 passed / 0 failed。

**上一轮的修复是正确的，不要回退、不要重构、不要改变方向。**

本轮是**复核后发现的 3 个残留缺口的补丁轮**。方案文档：
`docs/superpowers/plans/2026-08-25-tool-call-identity-and-anomaly-dimensions-patch.md`

前一轮方案（背景参考，**不需要再实施**）：
`docs/superpowers/plans/2026-08-25-tool-call-arguments-concat-account-switch.md`

**你的任务是实施这 3 个补丁，不是诊断，也不是重做上一轮。**

---

## 缺口 1（最高优先）：`name` 未参与工具调用身份判定

### 故障链（已闭环，三步，全部为代码事实）

1. 上游发来一个**真正的新工具调用**分片，但既无 `index` 又无 `id`（这是本次缺陷上游的真实风格，不是假设）。此刻恰有 1 个未完成条目。
2. `src/protocol.rs:3182-3188` 的 `open == 1` 分支判定为「续写」，返回**旧条目的键**。`src/protocol.rs:3235` 的 `if entry.name.is_none()` 因为旧条目已有 name，**静默丢弃新片段的 name**。
3. `merge_tool_call_arguments`（`src/protocol.rs:2612`）发现 `entry.arguments` 已是完整 JSON、新片段以 `{` 开头 → 判 `complete_then_new` → **clear + 覆盖**。

**结果**：两个工具调用塌成一个，`name` 来自调用 A、`arguments` 来自调用 B。

**为什么必须优先修**：原缺陷表现为上游 400（响亮失败，看得见）；本缺口表现为**用错误参数执行了错误的工具**（静默失败）。

**为什么修起来很便宜**：`extract_tool_call_details`（`src/protocol.rs:2012`）返回的就是 `(Option<String>, String)`，`name` 是**已经在手的免费身份信号**，当前代码只是没拿它做判定。

### 要做什么

**1.1** 在 `src/protocol.rs:3182-3188` 的 `open == 1` 分支加 name 兼容性闸门。

「片段 name 缺失」定义为 `None` **或** `Some("")`（部分上游在续传分片上发 `"name": ""`，必须与真正缺失同等对待）。

| 片段 name | 开放条目 `entry.name` | 判定 |
|---|---|---|
| 缺失 | 任意 | **续写**（合并到该条目）——**这是正常续传分片的形状，必须保持现有行为** |
| `Some(n)` | 缺失 | **续写**，`:3235` 现有逻辑补上 name |
| `Some(n)` | `Some(n)` 相同 | **续写** |
| `Some(n)` | `Some(m)`，`n != m` | **新调用**：键取 `self.tool_calls.keys().next_back().map_or(0, \|max\| max + 1)`，打 anomaly `reason="name_mismatch"`，`call_id` 传**开放条目的 `call_id`** |

`open != 1` 的分支**不要改**（已有 `ambiguous_continuation` + `max+1`）。

**1.2** 在 `src/protocol.rs:3232-3236`：当 `entry.name` 为 `Some(existing)`、片段 name 为 `Some(incoming)` 且两者不同时，打 anomaly `reason="index_name_mismatch"`。

**必须只打点、不改 `entry.name`** —— `response.output_item.added` 可能已用旧 name 发给下游，中途改名会让客户端状态不一致。

**1.3** 1.1 位于 `tool_call_merge_strict`（默认开）的 strict 分支内，随该开关回退。1.2 是纯日志。**不要新增运行时开关。**

---

## 缺口 2：anomaly 事件在请求方向丢失全部归属维度

### 现状

三处调用点把观测参数传成 `None`：

| 位置 | 函数 |
|---|---|
| `src/protocol.rs:1734-1740` | `response_function_call_item_to_chat_tool_call` |
| `src/protocol.rs:1944-1950` | `chat_function_call_to_function_call` |
| `src/protocol/tool_adapter.rs:203-205` | `adapt_responses_function_call` |

`normalize_tool_arguments_for_request`（`src/protocol.rs:1696`）把这两个参数转交 `log_tool_call_arguments_anomaly`（`src/protocol.rs:2651`），后者用它们填 `model` / `request_id` / `upstream_id`。传 `None` ⇒ 三个字段全部空串。

**后果**：请求方向（**含存量污染历史重放这条最重要的路径**）发出的 anomaly 无法归因到账号。上一轮方案 §4.4 第 2、3 条验收标准依赖这些维度，在这条路径上等于没有观测。流方向（`emit_tool_call_done:3092`、`completed_output_items:3472`、`merge_tool_call_arguments:2627`）维度是齐的，所以不是全盲。

**根因是签名形状**：这三个是自由函数，签名里只有 `tool_arguments_strict: bool`，拿不到 `TranslatorDiagnostics`。

### 要做什么

**2.1** 在 `src/protocol.rs` 新增：

```rust
#[derive(Clone, Copy, Debug, Default)]
pub struct ToolArgumentsContext<'a> {
    pub strict: bool,
    pub model: Option<&'a str>,
    pub request_id: Option<&'a str>,
    pub upstream_id: Option<&'a str>,
}
```

**2.2** `ConversionContext`（`src/protocol.rs:355`）新增三个 `Option<String>` 字段 `model` / `request_id` / `upstream_id`；三个既有构造器（`new:366`、`reasoning_content:378`、`Default:390`）一律初始化为 `None`；新增 `pub fn tool_arguments_context(&self) -> ToolArgumentsContext<'_>`。

**2.3** `normalize_tool_arguments_for_request(raw, strict, call_id, model, diagnostics)` → `(raw, ctx: ToolArgumentsContext<'_>, call_id)`。把当前透传 `tool_arguments_strict: bool` 的整条调用链换成 `ToolArgumentsContext<'_>`。需要改签名的位置：

- `src/protocol.rs`：`:742`、`:748`、`:1075`、`:1084`、`:1152`、`:1166`、`:1195`、`:1222`、`:1240`、`:1460`、`:1505`、`:1576`、`:1587`、`:1718`、`:1734`、`:1926`、`:1944`
- `src/protocol/tool_adapter.rs`：`:171`、`:203`、`:205`

**这是机械改造。不要在这个过程里顺手改任何行为逻辑。**

**2.4** `log_tool_call_arguments_anomaly`（`src/protocol.rs:2651`）签名改为直接收 `model` / `request_id` / `upstream_id` 三个 `Option<&str>`（取代现在的 `model` + `diagnostics`）。流方向三个调用点从 `self.diagnostics` 取值适配。**目的是两条路径的事件字段完全一致**，运维一个查询覆盖全部 anomaly。

**2.5** dispatch 路径填充：

- `responses_request_to_chat_payload_with_fallback`（`src/server/gateway/responses_fallback.rs:41`）增加 `model` / `request_id` / `upstream_id` 入参，在 `:109`（`conversion_context.tool_arguments_strict = ...` 旁边）写入 context。
- 调用点 `src/server/gateway/upstream.rs:1498` 传 `&request_id`、`&upstream.id`、`&final_upstream_model` —— **这三个变量在该处已在作用域内**（`upstream.rs:1481-1485` 的 `tracing::warn!` 在用 `request_id` / `model`，`upstream.rs:1409` 在用 `upstream.id`），不需要新建通道。
- 其他非 dispatch 构造点（测试、工具）保持 `None`。

---

## 缺口 3：rustfmt 未通过

`rtk cargo fmt --all -- --check` **实际不通过**：`tests/gateway/responses/session_recovery.rs:1143` 与 `:1250` 两处 `assert_eq!` 需要换行展开。上一轮回填表把 fmt 标成了 ✅，与事实不符。

执行 `rtk cargo fmt --all`。**若它改动了这两处之外的文件，停下来报告，不要合并进本轮提交。**

---

## 必须新增的测试

### 缺口 1 守卫（三条）

| 测试 | 构造 | 断言 |
|---|---|---|
| **name 不同 ⇒ 拆成两个调用** | 分片1：`index=0`, `id="call_a"`, `name="shell"`, `arguments="{\"command\":[\"ls\"]}"`；分片2：**无 index、无 id**，`name="apply_patch"`, `arguments="{\"patch\":\"x\"}"` | 收到**两个** `response.function_call_arguments.done`；`shell` 那个是 `{"command":["ls"]}`，`apply_patch` 那个是 `{"patch":"x"}`。**两者都不得丢失** |
| **name 缺失 ⇒ 仍续写（防回归）** | 与既有 `fragmented_tool_call_without_index_id_yields_valid_done_arguments`（`tests/gateway/responses/streaming.rs:1736`）完全相同的形状 | 该既有测试**必须继续通过，不得修改其断言** |
| **name 空串 ⇒ 视为缺失，仍续写** | 分片2 带 `"name": ""` + 真实 arguments | 只有一个 done 事件，`arguments` 为 `{"command":["ls"]}` |

### 缺口 2 验收（一条）

构造一次带 `previous_response_id` 的重放，历史里塞入 `arguments` 为 `"{}{\"command\":[\"ls\"]}"` 的 `function_call` item（模拟存量污染），走 Responses→Chat 转换路径。断言：

1. 实际发往上游的 `tool_calls[].function.arguments` 为 `{"command":["ls"]}`；
2. 日志中存在 `event="tool_call_arguments_anomaly"`、`reason="trailing_data"`，且 **`upstream_id`、`model`、`request_id` 三字段均非空**。

**第 2 条是本任务的核心验收点，不允许只断言第 1 条。**

### delta/done 一致性回归（一条）

上一轮引入的 `client_delta_desynced`（`src/protocol.rs:3260-3264`、`:3404-3408`）在发生覆盖时停止下发 delta，依赖客户端采信 `function_call_arguments.done` 的完整值。该取舍方向是对的，但目前**无测试守卫**。

构造一个**正常**（无 anomaly）的多分片工具调用流，断言：同一 `item_id` 的所有 `response.function_call_arguments.delta` 片段按序拼接后，**逐字节等于**该 `item_id` 的 `done` 事件的 `arguments`。

作用是保证 desync 抑制**没有污染正常路径**。

---

## 约束

1. **所有命令必须带 `rtk` 前缀，命令链中每一段都要带**（含 `&&` 链）：
   ```bash
   rtk git add . && rtk git commit -m "..." && rtk git push
   ```

2. **验证链逐条跑，逐条确认结果**：
   ```bash
   rtk cargo fmt --all
   rtk cargo fmt --all -- --check
   rtk cargo clippy --all-targets --all-features -- -D warnings
   rtk cargo test
   ```
   **不要用 `&&` 把 fmt 检查串在最前面就交差** —— 上一轮正是因为 fmt 挂掉，导致 clippy 与 test 在实施者本地从未执行，回填表却写了 ✅。

3. **提交粒度**：
   - 缺口 1（P1.1 + P1.2 + 三条守卫测试）一个提交；
   - 缺口 2 的 P2.3 机械改签名**单独成提交**，提交内**不得含任何 `if` / 策略逻辑变更**（便于 review 只看签名透传）；P2.1/2.2/2.4/2.5 + 验收测试可另成一到两个提交；
   - 缺口 3 单独提交。

4. **不要碰**：
   - `upstream_retry_after_cap_seconds` / `retry_max_wait` / `max_rounds` 及路由重试相关代码 —— 那是**另一个独立缺陷**，方案在 `docs/superpowers/plans/2026-08-25-route-exhaustion-cooldown-budget-invariant.md`，本轮不实施；
   - T1.3（主键改 `call_id`）、T3.4（历史键加 profile 维度）—— 均已论证延后，**不要主动做**；
   - `src/protocol.rs:3924`（`ResponsesToolCallState` 反方向累加）—— 该处主键是上游提供的 `output_index`（真实身份），无位置兜底碰撞，已记录为已知残留面，本轮不处置。

5. **完成后回填**方案文档 `docs/superpowers/plans/2026-08-25-tool-call-identity-and-anomaly-dimensions-patch.md` §6 的 commit hash 与 ✅ 状态。回填的验证链结果**必须是你实际跑出来的**，不得照抄。

6. **验收标准**（方案 §3.5）：fmt 通过；`rtk cargo test` 全绿且总数 ≥ 1745 + 新增；既有守卫测试未被修改；请求方向 anomaly 带全三个维度。
