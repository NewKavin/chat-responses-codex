# 高保真客户端兼容矩阵设计

## 摘要

为 `test` 下游构建一套可重复执行的客户端兼容矩阵，证明它当前暴露的每个模型都能通过用户关心的三类客户端链路发起调用：

- Codex
- opencode
- Hermes

第一版的优先级是“尽量保真，但在上游强制拒绝时优先保证请求可执行”。也就是说，网关在第一跳应尽可能保留 Responses 语义；只有当命中的上游只支持 `ChatCompletions`，且明确拒绝这些语义时，才按阶段逐层降级。

这份设计不把多个模型 slug 收敛成一个 canonical alias。只要下游当前暴露了多个 slug，即使它们属于同一模型家族，也必须继续分别展示、分别验证、分别保证可用。

## 目标

- 让 `test` 下游当前暴露的每个模型都能通过以下客户端链路调用：
  - Codex：走 `/v1/responses`
  - opencode：走 `/v1/chat/completions`
  - Hermes：走现有的 OpenAI-compatible chat 路径
- 在目标上游不支持原生 Responses 时，为 Codex 保留一条“尽量高保真”的 `Responses -> ChatCompletions` fallback 路径。
- 只要能够表达，标准 `function` 工具就要尽量保留并继续可用。
- 当目标上游只支持 `ChatCompletions` 时，对 `web_search`、`file_search`、`computer_use` 这类 Responses 内置工具要明确报不支持，而不是静默吞掉。
- 提供一套可重复执行的兼容矩阵：
  - 管理端可视化入口
  - 仓库内脚本化 smoke 入口
  这样后续每次改动都能复跑，及时发现回归。

## 非目标

- 不把多个模型 slug 合并成一个统一的下游模型名。
- 不新增第三种上游协议类型，仍只围绕现有 `ChatCompletions` 和 `Responses` 处理。
- 不为 Responses 内置工具伪造“看起来成功”的 chat-only 降级语义。
- 不重做下游鉴权、上游管理、模型探测页等其它系统。
- 第一版不要求先引入 provider 专属配置后才能落地。

## 当前状态

当前网关对目标客户端所需的协议面已经齐备：

- Codex 使用 `/v1/responses`
- opencode 使用 `/v1/chat/completions`
- Hermes 使用现有 OpenAI-compatible chat 路径

因此核心问题不是缺少 endpoint，而是 fallback 行为在高保真和可执行之间还没有被设计清楚。

目前已经观察到的失败类型有四类：

1. **chat-only 上游 fallback 时保留了过多 Responses 状态**
   - 近期改动使 `Responses -> ChatCompletions` fallback 会把巨大的 `previous_response_id` 历史、工具状态和工具输出块继续重放给 chat-only 上游。
   - 结果是同一个上游上，小型 chat 请求能成功，但高保真 fallback 却会触发 `CONTENT_LENGTH_EXCEEDS_THRESHOLD`、`TOOL_CONFIG_MISSING` 之类的 4xx。

2. **第三方 chat proxy 会拒绝部分 OpenAI / Responses 扩展字段**
   - 至少已经确认 `parallel_tool_calls` 在某些 chat-only proxy 上会触发 400。
   - 这类字段不能默认长期透传到所有第三方 `ChatCompletions` upstream。

3. **同一家模型家族的不同 slug 会命中不同上游**
   - 例如一个活跃上游只暴露 `deepseek-v4-flash`，另一个活跃上游只暴露 `deepseek-ai/deepseek-v4-flash`。
   - 第一版必须保留这种差异，不能靠合并 slug 来掩盖问题，所以兼容性要按 slug 单独验证。

4. **部分上游在 chat 面健康，但在语义上并不稳定**
   - 例如某些 Claude slug 在某个 chat-only upstream 上会返回语法正确的 `200`，但内容为空、token 全为 0。
   - 这属于上游行为问题，但网关必须明确分类，不能和协议转换失败混为一谈。

## 设计原则

1. **优先保留语义，只有在证据明确时才降级**
   - 第一跳应该尽量保留 Responses 语义。
   - 只有在上游给出明确的协议/字段拒绝信号时，才按阶段剥离高风险语义。

2. **客户端兼容性必须在网关真实边界上验证**
   - 兼容矩阵必须走真实的 `/v1/*` 网关入口和真实下游鉴权。
   - 不能绕过路由、配额、stream 逻辑，也不能伪造“本地直连上游成功”来代替网关兼容。

3. **按 slug 保证兼容，而不是按模型家族抽象兼容**
   - 如果 `deepseek-v4-flash` 可用、`deepseek-ai/deepseek-v4-flash` 不可用，矩阵必须明确展示这种差异。
   - 第一版不能通过静默重写 slug 把问题藏起来。

4. **无法映射的内置工具必须显式失败**
   - `web_search`、`file_search`、`computer_use` 这类 Responses 内置工具无法忠实映射到 chat-only upstream。
   - 系统必须直接说明“不支持”，而不是假装成功。

## 客户端矩阵范围

### Codex

Codex 必须通过 `/v1/responses` 验证。

必测项：

- 基础非流式请求
- 基础流式请求
- 标准 `function` 工具请求
- 长历史请求
- `previous_response_id` 续写请求
- 流式 continuation / replay 场景

Codex 特有说明：

- `previous_response_id` 是 Responses 专有语义，因此只要求在 Codex 链路上验证。

### opencode

opencode 必须通过 `/v1/chat/completions` 验证。

必测项：

- 基础非流式请求
- 基础流式请求
- 标准 `function` 工具请求
- 长多轮历史请求
- 能映射到 chat 语义的 replay 等价场景

### Hermes

Hermes 第一版按 OpenAI-compatible chat 客户端处理，沿用现有 `scripts/hermes.sh` 路径。

必测项：

- 基础非流式请求
- 基础流式请求
- 标准 `function` 工具请求
- 长多轮历史请求

## Fallback 梯度

这套梯度只在以下前提下生效：

- 下游请求是 `Responses`
- 当前模型没有任何活跃的 `Responses` upstream 候选
- 因此只能退到 `ChatCompletions` upstream

### 第 0 阶段：高保真尝试

第一跳发送尽可能完整的 `Responses -> ChatCompletions` 转换结果，保留：

- 当前轮用户输入
- `instructions` / system prompt
- 标准 `function` 工具
- 可表达的 `tool_choice`
- 可兼容的 `reasoning_effort`
- 可表达的 `response_format` / JSON schema
- 已知安全的 `stream_options`
- 若存在，则保留重放历史与 `previous_response_id` 状态

### 第 1 阶段：扩展字段清理

如果上游返回的是协议/字段形状的 4xx，先清理高风险扩展字段：

- `parallel_tool_calls`
- `stream_options.include_obfuscation`
- 其它已知会触发第三方 chat proxy 拒绝的 Responses / OpenAI 扩展字段

### 第 2 阶段：工具重放缩减

如果上游继续因工具/Schema 类 4xx 拒绝请求，则尽量保留当前轮的标准 `function` 工具，但删除：

- 重放出来的工具输出
- 重放出来的工具调用状态
- 带 `tool_call_id` 的 chat 消息
- 由 `previous_response_id` 派生出的 assistant tool-call block

这一阶段仍应保留当前轮用户请求，以及安全的 system prompt 和可保留的纯文本历史。

### 第 3 阶段：历史压缩

如果上游仍然因为上下文长度或请求体体积拒绝请求，则将 fallback 压缩到最接近操练场可工作的 chat 语义：

- 保留 system / instructions（如果存在）
- 只保留必要的纯文本 user / assistant 历史
- 删除 `previous_response_id`
- 删除 replay 专用的 Responses 状态

### 第 4 阶段：显式失败

如果上游到这一步仍然拒绝：

- 返回真实的网关分类错误
- 记录本次到达的 fallback 阶段
- 在兼容矩阵里明确显示失败发生在哪一层

### 重复失败记忆

对以下元组维护一份内存内的“高保真拒绝计数”：

- downstream id
- client family
- model slug
- upstream id
- fallback stage

策略：

- 第一次失败后，后续相同调用仍然继续尝试更高保真阶段。
- 相同元组的高保真失败最多重试 3 次。
- 当同一元组在同一阶段累计失败达到 3 次后，后续相同调用直接跳过这个已证明不可行的高保真阶段，从下一层较低阶段开始。
- 只要任一较高保真阶段成功一次，就重置对应的失败记忆。

这可以保证系统一开始是探索式的，但不会永远为同一种失败路径反复付出成本。

## 不支持的内置工具

当命中的路径只能 fallback 到 chat-only upstream 时：

- 标准 `function` 工具仍应尽量支持。
- `web_search`、`file_search`、`computer_use` 必须返回明确的不兼容错误。

错误文案至少需要说明：

- 当前模型 / upstream 路径只支持 `ChatCompletions`
- 请求中的内置 Responses 工具无法忠实映射
- 用户需要更换模型 / upstream，或者移除这类内置工具

## 诊断入口

### 管理端矩阵

新增一个管理端兼容矩阵入口，能够：

- 选择下游（第一版默认覆盖 `test` 用例）
- 选择一个或多个客户端家族
- 对该下游当前通过 `/v1/models` 暴露出来的全部模型批量执行兼容检查

每个 “模型 x 客户端” 单元格至少展示：

- 使用的 endpoint
- 选中的 upstream
- 实际走的是 `native` 还是 `responses_to_chat`
- 到达的 fallback 阶段
- 最终状态：`passed` / `warning` / `failed`
- 失败时的网关错误分类
- 简洁摘要和下一步建议

### 脚本化 smoke

在仓库内提供一个可脚本化执行的 smoke 入口，能够：

- 从本地部署实例解析目标下游 key
- 拉取该下游实时 `/v1/models`
- 对 `codex`、`opencode`、`hermes` 执行同一套兼容矩阵
- 输出机器可读结果和人类可读摘要

这个脚本是后续所有兼容性回归的基础验收工具，不能依赖管理端 UI 才能使用。

## 结果模型

每条矩阵结果至少包含：

- `client_family`
- `model_slug`
- `endpoint`
- `selected_upstream_id`
- `selected_upstream_name`
- `selected_upstream_protocol`
- `protocol_transition`
- `fallback_stage`
- `status`
- `http_status`
- `gateway_error_category`
- `summary`
- `details`
- `duration_ms`

可选调试字段：

- `safe_payload_metrics`
  - message 数量
  - tool 数量
  - 是否包含 `reasoning_effort`
  - 是否包含 `stream_options`
- `skipped_stages`
  - 因“同类失败已累计 3 次”而被自动跳过的更高保真阶段

## 验证计划

必须补齐的自动化检查：

- `protocol.rs` 单元测试，覆盖新的 fallback-safe 转换逻辑
- `gateway` 集成测试，覆盖：
  - 高保真 fallback 成功
  - 命中协议类 4xx 后的分层降级
  - 同类失败 3 次后的阶段跳过
  - chat-only upstream 上对 Responses 内置工具的显式失败
  - 在不合并 slug 的前提下，对每个 slug 单独验证矩阵结果

必须补齐的运行时验证：

- 针对 `test` 下游，对全部 live model slug 执行客户端矩阵
- 至少专项覆盖：
  - 一个 Claude 家族的 chat-only slug
  - 一个 DeepSeek 家族的 chat-only slug
  - 一个当前已知能通过 Codex fallback 运行的模型

## 风险

- 在 chat-only upstream 上尽量保留高保真 Responses 语义，会提高 payload 复杂度；如果梯度策略太宽松，可能再次引入 4xx。
- 某些上游会在“空成功包 / 4xx / 5xx”之间波动，矩阵必须能把“协议问题”和“provider 本身不稳定”区分开。
- 保留多个 slug 分别暴露意味着运维需要继续理解：即使是同一家模型家族，不同 slug 也可能路由到不同 upstream。

## 开放问题

本版本没有额外开放问题。当前已确认的边界是：

- 保留多个 slug 分别展示
- 尽量保留高保真 Responses 语义
- 必须优先保证请求可执行
- 对 chat-only upstream 上无法映射的 Responses 内置工具显式报错
