# 工具调用身份判定与 anomaly 维度补齐（`extra data` 修复的补丁轮）

> 前置：本文档是 `2026-08-25-tool-call-arguments-concat-account-switch.md` 的**补丁轮**。
> 前一轮（commit `1389e808` → `49246447`）已修掉 `extra data: line 1 column 3 (char 2)` 的根因，
> 代码核查通过、clippy 干净、`rtk cargo test` 1745 passed / 0 failed。
> 本轮处理复核中发现的 **3 个残留缺口**，其中 P1 会产生**静默错参**，优先级高于一切。

---

## 0. 报错来源

本轮**不是**由新的线上报错驱动，而是对前一轮实施结果做代码级复核后发现的缺口。三条来源：

1. **P1（静默错参风险，未在生产观测到，但路径可达）** —— 前一轮 T1.1 引入的「唯一未完成条目即视为续写」规则，与既有的 name 吸收逻辑叠加后，会把一个**真正的新工具调用**误并入上一个调用。
2. **P2（验收指标失效）** —— 前一轮 §4.4 第 2 条把 `tool_call_arguments_anomaly` 的 `upstream_id` 维度定义为「判断哪个账号分片风格异常的唯一手段」，但请求方向的三处调用点把 `model` / `diagnostics` 传成了 `None`，该维度在这条路径上为空。
3. **P3（CI 阻塞）** —— `rtk cargo fmt --all -- --check` 实际不通过。前一轮回填表标记为 ✅，与事实不符。

---

## 1. 根因

### 1.1 P1：`name` 未参与工具调用身份判定

**位置**：`src/protocol.rs:3182-3188`（`emit_chat_tool_call_delta` 中 strict 分支的 `open == 1` 分支）与 `src/protocol.rs:3235`（name 吸收）。

现状代码（`:3182-3188`）：

```rust
match open {
    1 => self
        .tool_calls
        .iter()
        .find(|(_, state)| !state.done_emitted)
        .map(|(index, _)| *index)
        .expect("counted exactly one open call"),
    _ => { /* ambiguous_continuation + max+1 */ }
}
```

现状代码（`:3235`）：

```rust
if entry.name.is_none() {
    entry.name = name.clone();
}
```

**故障链**（三步，全部为代码事实）：

1. 上游发来一个**真正的新工具调用**分片，但既无 `index` 又无 `id`（这正是本次缺陷的上游风格，不是假设）。此刻恰有 1 个未完成条目。
2. `open == 1` 分支判定为「续写」，返回**旧条目的键** → `entry(旧键)` 命中。`:3235` 因为 `entry.name` 已是 `Some(旧名)`，**静默丢弃新片段的 name**。
3. `merge_tool_call_arguments`（`:2612`）发现 `entry.arguments` 已是完整 JSON、新片段以 `{` 开头 → 判 `complete_then_new` → **clear + 覆盖**。

**结果**：两个工具调用塌成一个，`name` 来自调用 A、`arguments` 来自调用 B。

**为什么比原缺陷更危险**：原缺陷表现为上游 400（响亮失败，能看见）；本缺口表现为**用错误的参数执行了一个错误的工具**（静默失败）。会打 `complete_then_new` anomaly，所以**可观测**，但结果已经错了。

**为什么现在能修**：`extract_tool_call_details`（`:2012`）返回的就是 `(Option<String>, String)`，`name` 是**已经在手的免费身份信号**。当前代码只是没用它做判定。

> T1.3（主键改 `call_id`）是本问题的结构解，前一轮按计划延后，**本轮继续延后**（理由不变：T1.1+T1.2 已止血，改主键涉及 `output_index` 复用、done 遍历顺序、`sort_by_key` 三处联动）。本轮用 `name` 做判定是**低成本、高收益的加固**，不是 T1.3 的替代品。

### 1.2 P2：anomaly 事件在请求方向丢失全部归属维度

**位置**：三处调用点把观测参数传成 `None`：

| 文件:行 | 函数 | 现状 |
|---|---|---|
| `src/protocol.rs:1734-1740` | `response_function_call_item_to_chat_tool_call` | `normalize_tool_arguments_for_request(raw_arguments, tool_arguments_strict, call_id, None, None)` |
| `src/protocol.rs:1944-1950` | `chat_function_call_to_function_call` | 同上形状 |
| `src/protocol/tool_adapter.rs:203-205` | `adapt_responses_function_call` | 同上形状 |

`normalize_tool_arguments_for_request`（`protocol.rs:1696`）把这两个参数直接转交 `log_tool_call_arguments_anomaly`（`protocol.rs:2651`），后者用它们填充 `model` / `request_id` / `upstream_id`。传 `None` ⇒ 三个字段全部渲染为空串。

**后果**：请求方向（含**存量污染历史重放**这条最重要的路径）发出的 `trailing_data` / `unparseable` anomaly **无法归因到账号**。前一轮 §4.4 验收标准第 2、3 条都依赖这些维度，在这条路径上等于没有观测。

流方向（`emit_tool_call_done:3092`、`completed_output_items:3472`、`merge_tool_call_arguments:2627`）维度齐全，所以不是全盲——但存量污染的清理过程恰好主要走请求方向。

**根因是签名形状**：这三个函数是自由函数，签名里只有 `tool_arguments_strict: bool`，拿不到 `TranslatorDiagnostics`。而 `ConversionContext`（`protocol.rs:355`）是每请求构造一次的天然载体，且已经承载了 `tool_arguments_strict`。

**可行性已确认**：`upstream.rs:1498` 的调用点上 `request_id`、`upstream.id`、`final_upstream_model` 均在作用域内（`upstream.rs:1481-1485` 的 `tracing::warn!` 已在用 `request_id` / `model`；`upstream.rs:1409` 已在用 `upstream.id`）。

### 1.3 P3：rustfmt 未通过

`tests/gateway/responses/session_recovery.rs:1143` 与 `:1250`，两处 `assert_eq!` 需要换行展开。

---

## 2. 开发任务

### P1【最高优先】让 `name` 参与身份判定

**P1.1 `open == 1` 分支增加 name 兼容性闸门**（`src/protocol.rs:3182-3188`）

「片段 name 缺失」定义为 `None` **或** `Some("")`（部分上游在续传分片上发 `"name": ""`，必须与真正缺失同等对待）。

判定表（仅作用于 strict 模式下、`index` 与 `id` 均缺失、`open == 1` 的分支）：

| 片段 name | 开放条目 `entry.name` | 判定 | 行为 |
|---|---|---|---|
| 缺失 | 任意 | **续写** | 合并到该条目（**这是正常续传分片的形状，必须保持现有行为**） |
| `Some(n)` | 缺失 | **续写** | 合并；`:3235` 现有逻辑补上 name |
| `Some(n)` | `Some(n)`（相同） | **续写** | 合并 |
| `Some(n)` | `Some(m)`，`n != m` | **新调用** | 键取 `self.tool_calls.keys().next_back().map_or(0, |max| max + 1)`；打 anomaly `reason="name_mismatch"`，`call_id` 传**开放条目的 `call_id`**（便于关联被误并的目标） |

`open != 1` 的分支**不改**（已有 `ambiguous_continuation` + `max+1`）。

**P1.2 `index` 存在但 name 冲突时的纯观测打点**（`src/protocol.rs:3232-3236`）

当 `entry.name` 为 `Some(existing)`、片段 name 为 `Some(incoming)` 且 `incoming != existing` 时，打 anomaly `reason="index_name_mismatch"`。

**必须只打点、不改 `entry.name`**：`response.output_item.added` 可能已用旧 name 发给下游，中途改名会让客户端状态不一致。这条路径在上游**提供了 `index` 但把它复用给了另一个调用**时可达，属于上游行为异常，先观测再决定是否处置。

**P1.3 开关归属**

P1.1 位于 `tool_call_merge_strict`（默认**开**）的 strict 分支内，随该开关一同回退。P1.2 是纯日志，不加开关。

**不要**新增运行时开关。

### P2 anomaly 事件补齐归属维度

**P2.1 引入借用式观测上下文**

在 `src/protocol.rs` 新增：

```rust
#[derive(Clone, Copy, Debug, Default)]
pub struct ToolArgumentsContext<'a> {
    pub strict: bool,
    pub model: Option<&'a str>,
    pub request_id: Option<&'a str>,
    pub upstream_id: Option<&'a str>,
}
```

**P2.2 `ConversionContext` 承载观测字段**

`ConversionContext`（`protocol.rs:355`）新增三个 `Option<String>` 字段：`model`、`request_id`、`upstream_id`。三个既有构造器（`new:366`、`reasoning_content:378`、`Default:390`）一律初始化为 `None`。新增方法：

```rust
pub fn tool_arguments_context(&self) -> ToolArgumentsContext<'_>
```

**P2.3 替换参数形状**

- `normalize_tool_arguments_for_request(raw, strict, call_id, model, diagnostics)` → `normalize_tool_arguments_for_request(raw, ctx: ToolArgumentsContext<'_>, call_id)`。
- 把当前透传 `tool_arguments_strict: bool` 的调用链整体换成 `ToolArgumentsContext<'_>`。需要改签名的位置（`bool` 参数所在）：
  `protocol.rs` 的 `:742`、`:748`、`:1075`、`:1084`、`:1152`、`:1166`、`:1195`、`:1222`、`:1240`、`:1460`、`:1505`、`:1576`、`:1587`、`:1718`、`:1734`、`:1926`、`:1944`；
  `protocol/tool_adapter.rs` 的 `:171`、`:203`、`:205`。
  这是机械改造，**不要在此过程中顺手改任何行为**。

**P2.4 统一 anomaly 事件形状**

`log_tool_call_arguments_anomaly`（`protocol.rs:2651`）签名改为直接接收 `model` / `request_id` / `upstream_id` 三个 `Option<&str>`（取代现在的 `model` + `diagnostics`）。流方向的三个调用点从 `self.diagnostics` 取值适配。

**目的**：两条路径发出的事件字段完全一致，运维侧一个查询就能覆盖全部 anomaly，无需区分来源。

**P2.5 在 dispatch 路径填充**

- `responses_request_to_chat_payload_with_fallback`（`server/gateway/responses_fallback.rs:41`）增加 `model` / `request_id` / `upstream_id` 入参，在 `:109`（`conversion_context.tool_arguments_strict = ...` 旁边）一并写入 context。
- 调用点 `server/gateway/upstream.rs:1498` 传入 `&request_id`、`&upstream.id`、`&final_upstream_model`。
- 其他非 dispatch 构造点（测试、工具）保持 `None`，事件字段渲染为空串，与现状一致。

### P3 修复 rustfmt

执行 `rtk cargo fmt --all`。预期只改动 `tests/gateway/responses/session_recovery.rs:1143` 与 `:1250` 两处。**若 fmt 改动了其他文件，停下来报告，不要合并进本轮提交。**

---

## 3. 测试要求

### 3.1 P1 守卫测试（必须新增，三条）

| 测试 | 构造 | 断言 |
|---|---|---|
| **name 不同 ⇒ 拆成两个调用** | 分片1：`index=0`, `id="call_a"`, `name="shell"`, `arguments="{\"command\":[\"ls\"]}"`；分片2：**无 index、无 id**，`name="apply_patch"`, `arguments="{\"patch\":\"x\"}"` | 收到**两个** `response.function_call_arguments.done`；名为 `shell` 的那个 `arguments` 为 `{"command":["ls"]}`，名为 `apply_patch` 的那个为 `{"patch":"x"}`。**两者都不得丢失** |
| **name 缺失 ⇒ 仍然续写（防回归）** | 与前一轮 `fragmented_tool_call_without_index_id_yields_valid_done_arguments` 完全相同的分片形状 | 该既有测试必须**继续通过**，不得修改其断言 |
| **name 为空串 ⇒ 视为缺失，仍然续写** | 分片2 带 `"name": ""` + 真实 arguments，其余同上 | 只有一个 done 事件，`arguments` 为 `{"command":["ls"]}` |

### 3.2 P2 验收测试（必须新增，一条）

构造一次带 `previous_response_id` 的重放，历史里塞入一个 `arguments` 为 `"{}{\"command\":[\"ls\"]}"` 的 `function_call` item（模拟存量污染），走 Responses→Chat 转换路径。

断言：
1. 实际发往上游的 `tool_calls[].function.arguments` 为 `{"command":["ls"]}`（可解析、无 `{}` 前缀）；
2. 捕获日志中存在 `event="tool_call_arguments_anomaly"`、`reason="trailing_data"`，且 **`upstream_id`、`model`、`request_id` 三个字段均非空**。

第 2 条是本任务的核心验收点，**不允许只断言第 1 条**。

### 3.3 delta/done 一致性回归（必须新增，一条）

前一轮引入的 `client_delta_desynced`（`protocol.rs:3260-3264`、`:3404-3408`）在发生覆盖时**停止下发 delta**，依赖客户端采信 `function_call_arguments.done` 的完整值。该取舍方向正确（继续下发会在客户端重建拼接），但目前**无测试守卫**。

构造一个**正常**（无 anomaly）的多分片工具调用流，断言：
- 把同一 `item_id` 的所有 `response.function_call_arguments.delta` 片段按序拼接，结果**逐字节等于**该 `item_id` 的 `done` 事件的 `arguments`。

这条守卫的作用是保证 desync 抑制逻辑**没有污染正常路径**。

### 3.4 验证命令（**必须使用 rtk 前缀，命令链中每一段都要带**）

```bash
rtk cargo fmt --all
rtk cargo fmt --all -- --check
rtk cargo clippy --all-targets --all-features -- -D warnings
rtk cargo test
```

**注意**：不要用 `&&` 把 fmt 检查串在最前面就交差——前一轮正是因为 `fmt` 挂掉导致 clippy 与 test 在实施者本地从未执行。**四条命令逐条跑，逐条确认结果。**

### 3.5 验收标准

1. `rtk cargo fmt --all -- --check` 通过。
2. `rtk cargo test` 全绿，且总数不低于 1745 + 本轮新增测试数。
3. 前一轮的 `fragmented_tool_call_without_index_id_yields_valid_done_arguments` 未被修改且通过。
4. 请求方向发出的 anomaly 事件带全 `upstream_id` / `model` / `request_id`。
5. 上线后 `name_mismatch` 与 `index_name_mismatch` 计数**若非 0**，其 `upstream_id` 维度即指出哪个账号会复用位置/索引身份——这是新增的诊断能力，非 0 不代表修复失败。

---

## 4. 风险与回滚

| 风险 | 影响 | 处置 |
|---|---|---|
| P1.1 把合法续写误判为新调用 | 一个工具调用被拆成两个，参数不完整 | 仅在片段**显式携带**与开放条目**不同**的非空 name 时触发；合法续写分片不带 name（前一轮守卫测试即此形状）。回退：关 `tool_call_merge_strict` |
| P2.3 机械改签名时夹带行为改动 | 难以定位的回归 | 要求 P2 单独成提交，且提交内**不含任何 `if` / 策略逻辑变更**；review 时只看签名与透传 |
| P2.2 `ConversionContext` 加字段破坏既有构造 | 编译失败或字段被默认吞掉 | 三个构造器显式初始化为 `None`；`Default` 派生不可依赖 |
| P1.2 打点量过大 | 日志噪声 | `index_name_mismatch` 只在上游确实复用 index 时触发，正常上游为 0；若观测到刷屏，说明发现了真实的上游异常，应当报告而非降噪 |

**回滚粒度**：P1 关 `tool_call_merge_strict` 即回到前一轮行为；P2 纯观测，无行为影响，可单独 revert；P3 无风险。

---

## 5. 明确不在本轮范围

| 项 | 理由 |
|---|---|
| T1.3 主键改 `call_id` | 结构解，涉及 `output_index` 复用（`protocol.rs:3212-3216`）、done 遍历顺序（`:3066-3126`）、`sort_by_key` 三处联动。等 `name_mismatch` / `ambiguous_continuation` 观测数据支撑后单独立项 |
| T3.4 历史键加 profile 维度 | 前一轮已论证：会用 `unknown previous_response_id` 400 换 `extra data` 400，取舍不成立 |
| `ResponsesToolCallState` 反方向累加点（`protocol.rs:3924`） | 该处主键为上游提供的 `output_index`（真实身份），不存在位置兜底碰撞，风险等级低于原缺陷。**记录为已知残留面**，不在本轮处置 |
| `upstream_routes_exhausted` | 独立缺陷，见 `2026-08-25-route-exhaustion-cooldown-budget-invariant.md`，**该方案尚未实施（零提交）** |

---

## 6. 回填表（实施后补充 commit hash 与 ✅）

| 任务 | commit | 状态 |
|---|---|---|
| P1.1 `open == 1` 分支 name 闸门 | `7f356b64` | ✅ |
| P1.2 `index_name_mismatch` 观测打点 | `7f356b64` | ✅ |
| P2.1 `ToolArgumentsContext` | `1c158f7d` | ✅ |
| P2.2 `ConversionContext` 观测字段 | `1c158f7d` | ✅ |
| P2.3 参数形状替换（机械） | `1c158f7d` | ✅ |
| P2.4 统一 anomaly 事件形状 | `f18a489` | ✅ |
| P2.5 dispatch 路径填充 | `f18a489` + `4b8969e` | ✅ |
| P3 rustfmt | `dfe725fa` | ✅ |

### 测试

| 测试 | 位置 | commit | 状态 |
|---|---|---|---|
| name 不同 ⇒ 拆成两个调用 | `tests/gateway/responses/streaming.rs:2136` `tool_call_name_mismatch_without_index_splits_into_two_calls` | `7f356b64` | ✅ |
| name 缺失 ⇒ 仍续写（既有测试不改） | `tests/gateway/responses/streaming.rs:1736` | 前一轮 `07a8626d` | ✅ |
| name 空串 ⇒ 视为缺失 | `tests/gateway/responses/streaming.rs:2176` `tool_call_empty_name_without_index_continues_open_call` | `7f356b64` | ✅ |
| 存量污染重放 + anomaly 维度非空 | `tests/gateway/responses/fallback.rs:2347` `polluted_replayed_history_repairs_and_anomaly_carries_dispatch_attribution` | `f18a489` | ✅ |
| delta/done 一致性 | `tests/gateway/responses/streaming.rs:2215` `normal_fragmented_tool_call_deltas_concatenate_bytewise_to_done_arguments` | `f18a489` | ✅ |

验证链（实际执行于隔离 worktree @ `4b8969e`，不含并行进程的路由耗尽 WIP）：

- `rtk cargo fmt --all -- --check` → **通过**
- `rtk cargo clippy --all-targets --all-features -- -D warnings` → **0 问题**
- `rtk cargo test` → **1749 passed / 0 failed / 88 ignored（62 suites, 298.60s）**

> 注：主工作树因并行进程（路由耗尽缺陷）的 WIP 会间歇性编译失败 / 3 个路由域测试失败
> （`upstream_5xx_with_nested_rate_limit_code_remains_transient` ×2、`route_exhaustion_budget_invariant`），
> 均与本轮改动无关；本轮全部新增测试在两种环境下均通过。
