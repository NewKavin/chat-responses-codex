# 内网部署 upstream_routes_exhausted 和 400 错误诊断

**日期：** 2026-08-25  
**问题：** 内网部署后仍出现 `upstream_routes_exhausted` 和间歇性 400 上游拒绝请求  
**受影响模型：** GLM5.1、deepseek-v4-flash-0731

---

## 1. 问题现状

用户报告：
- `upstream_routes_exhausted` 问题依旧存在
- 时不时出现 400 上游拒绝请求
- 主要使用 GLM5.1 和 deepseek-v4-flash-0731 模型

从 `2026-08-22-upstream-error-code-surfacing.md` 第 8 节的排查记录可知：
- K-API 对某些参数返回 HTTP 400，错误码 `invalid_parameter_error`
- 目标请求是 Responses → ChatCompletions 转换
- 文档第 8.4 节提出了 F1-F4 后续方案但标注为"未实施"

---

## 2. 代码审查发现

### 2.1 当前已有的方言重试机制

**位置：** `src/server/gateway/dialect_retry.rs`

```rust
pub fn correction_for_response(
    status: StatusCode,
    error_body: &[u8],
    response_started: bool,
    rules: &[DialectCorrectionRule],
) -> Option<DialectCorrectionRule>
```

- ✅ 已实现：针对 HTTP 400 + `invalid_parameter` / `unsupported_parameter` 的重试
- ✅ 已实现：基于 `DialectCorrectionRule` 的字段移除和 token limit 切换
- ✅ 已实现：通用降级 `generic_strip_field_for_response`（A3 机制）

**安全字段白名单：** `src/server/gateway/capability_probe.rs:638`
```rust
"parallel_tool_calls"
| "service_tier"
| "reasoning_effort"
| "max_output_tokens"
| "max_completion_tokens"
| "stream_options"
| "verbosity"
| "prompt_cache_key"
```

**问题：`logprobs` 和 `top_logprobs` 不在白名单中！**

### 2.2 Responses → Chat 转换

**位置：** `src/protocol.rs:525` `responses_request_to_chat_payload_with_context`

当前复制的字段：
```rust
copy_field(input, &mut output, "stream");
copy_field(input, &mut output, "temperature");
copy_field(input, &mut output, "top_p");
copy_field(input, &mut output, "stop");
copy_field(input, &mut output, "metadata");
copy_field(input, &mut output, "service_tier");
copy_field(input, &mut output, "store");
copy_field(input, &mut output, "safety_identifier");
copy_field(input, &mut output, "prompt_cache_key");
copy_field(input, &mut output, "prompt_cache_retention");
// ...
copy_field(input, &mut output, "parallel_tool_calls");
```

**发现：没有复制 `logprobs` 字段！**

但这不意味着问题不是 `logprobs`，因为：
1. Codex 可能在 Responses 请求中包含 `logprobs`
2. 如果包含了，转换时虽然不会被复制，但**可能通过其他路径进入请求**
3. 或者问题是其他参数（`reasoning_effort`、`parallel_tool_calls` 等）

### 2.3 保留字段检查

**位置：** `src/server/gateway/upstream.rs:458-459`

```rust
| "logprobs"
| "top_logprobs"
```

这里 `logprobs` 和 `top_logprobs` 被列为保留字段，用于防止与 reasoning_effort 字段冲突。

---

## 3. 根本问题分析

### 问题 1：`logprobs` 不在方言重试安全字段白名单中

即使上游返回 400 + `param=logprobs`，当前的 `generic_strip_field_for_response` 也**不会**自动移除它重试，因为：

```rust
pub(super) fn is_safe_dialect_strip_field(field: &str) -> bool {
    matches!(
        field,
        "parallel_tool_calls" | "service_tier" | ... 
        // ❌ 没有 "logprobs" | "top_logprobs"
    )
}
```

### 问题 2：GLM/Deepseek 可能不支持某些标准字段

国内模型（GLM、Deepseek、Qwen 等）通常：
- ❌ 不支持 `logprobs` / `top_logprobs`
- ❌ 不支持 `parallel_tool_calls`
- ❌ 不支持 `stream_options.include_usage`（部分）
- ❌ 对 `reasoning_effort` 的支持不一致
- ❌ 对 `max_output_tokens` vs `max_tokens` 的偏好不同

### 问题 3：能力探测可能不完整

当前能力探测不会主动测试所有方言字段组合，可能导致：
1. 某个路由被标记为可用
2. 但实际请求带了某个字段（如 `logprobs`）导致 400
3. 单次 400 会导致路由冷却
4. 所有路由都冷却 → `upstream_routes_exhausted`

### 问题 4：Responses → Chat fallback 缺少完整的字段过滤

Codex 发送 Responses 请求时可能包含：
- `logprobs` (在 Anthropic Responses API 中合法)
- 但转换到 Chat 后，某些上游不支持

---

## 4. 立即修复方案

### 修复 1：添加 `logprobs` 到安全字段白名单

**文件：** `src/server/gateway/capability_probe.rs:638`

```rust
pub(super) fn is_safe_dialect_strip_field(field: &str) -> bool {
    matches!(
        field,
        "parallel_tool_calls"
            | "service_tier"
            | "reasoning_effort"
            | "max_output_tokens"
            | "max_completion_tokens"
            | "stream_options"
            | "verbosity"
            | "prompt_cache_key"
            | "logprobs"              // ✅ 新增
            | "top_logprobs"          // ✅ 新增
    )
}
```

**原因：**
- `logprobs` 不会泄漏用户数据（只是一个布尔值或数字）
- 移除它是安全的
- OpenAI 兼容 API 标准字段

### 修复 2：在 Responses → Chat 转换时主动过滤 logprobs

**文件：** `src/server/gateway/responses_fallback.rs`

在 `responses_request_to_chat_payload_with_fallback` 中：

```rust
pub(super) fn responses_request_to_chat_payload_with_fallback(
    body: &Value,
    // ...
) -> Result<Value, ProtocolError> {
    let mut sanitized = body.clone();
    let mut tool_registry = ...;

    if let Some(object) = sanitized.as_object_mut() {
        // ✅ 新增：移除可能不兼容的字段
        object.remove("logprobs");
        object.remove("top_logprobs");
        
        // 现有逻辑...
    }
    
    // ...
}
```

**或者更保守：** 根据目标上游的能力档案决定是否保留

### 修复 3：增强能力探测覆盖

**建议：** 为国内模型（GLM、Deepseek）预设保守的能力档案

**文件：** `src/capabilities/policy.rs` 或通过配置

```json
{
  "model_pattern": "^(glm-|deepseek-|qwen-)",
  "reject_capabilities": [
    "logprobs",
    "top_logprobs",
    "parallel_tool_calls"
  ],
  "token_limit_field": "max_tokens"
}
```

---

## 5. 诊断步骤

### Step 1：确认是否是 logprobs 问题

```bash
# 查找最近的 400 错误，查看 param 字段
cd ~/docker/chat-responses-codex/logs
grep -a "status 400" chat-responses-codex.log | \
  grep -oP 'param=[^,}]+' | sort | uniq -c | sort -rn
```

### Step 2：查看具体的错误码

```bash
# 查找 invalid_parameter_error 出现次数
grep -a "invalid_parameter" chat-responses-codex.log | wc -l

# 查看最近的完整错误
grep -a "invalid_parameter" chat-responses-codex.log | tail -5
```

### Step 3：验证 routes exhausted 的触发模式

```bash
# 查看 routes exhausted 之前的路由冷却
grep -a "upstream_routes_exhausted" -B 20 chat-responses-codex.log | \
  grep "route_action\|failure_class"
```

---

## 6. 实施计划

### P1：紧急修复（立即实施）

- [ ] **P1.1** 添加 `logprobs` 和 `top_logprobs` 到 `is_safe_dialect_strip_field` 白名单
- [ ] **P1.2** 在 Responses → Chat 转换中移除 `logprobs` / `top_logprobs`（除非明确知道上游支持）
- [ ] **P1.3** 测试：Codex + GLM/Deepseek + 长会话 + 工具调用

### P2：诊断增强（1-2 天）

- [ ] **P2.1** 实施 F1（安全记录上游参数错误摘要）
  - 记录 `upstream_error_param` 而不仅仅是 `upstream_error_code`
  - 日志中包含 `rejected_param=logprobs` 这样的信息
- [ ] **P2.2** 增强能力探测，覆盖 `logprobs` 字段测试

### P3：完整方案（1 周）

- [ ] **P3.1** 实施 F2（字段级兼容重试）
- [ ] **P3.2** 实施 F3（参数拒绝写入能力档案）
- [ ] **P3.3** 实施 F4（明确不可重试语义）

---

## 7. 测试矩阵

| 场景 | 期望 |
|------|------|
| Codex Responses 请求 + GLM + logprobs | 自动移除 logprobs，请求成功 |
| Chat 请求 + Deepseek + logprobs | 第一次 400，自动重试无 logprobs，成功 |
| Chat 请求 + 不支持 parallel_tool_calls | 自动降级，成功 |
| 多路由全失败 + 不同 param 错误 | 记录每个 param，诊断明确 |
| 上游 400 + param=unknown_field | 如果在白名单，重试；否则直接失败 |

---

## 8. 风险评估

| 风险 | 缓解 |
|------|------|
| 移除 logprobs 导致客户端期望的数据缺失 | Codex 目前不依赖 logprobs；即使缺失也不影响核心功能 |
| 过度过滤导致合法字段被移除 | 仅在 400 + invalid_parameter 时重试，且有白名单保护 |
| 国内模型参数支持随版本变化 | 能力探测会自动学习；保守策略不会导致功能丢失 |

---

## 9. 下一步

1. **立即执行 P1.1 和 P1.2**
2. 部署到内网环境
3. 监控 24 小时，收集日志
4. 根据日志确认问题是否解决
5. 如果仍有问题，执行 P2 诊断增强
