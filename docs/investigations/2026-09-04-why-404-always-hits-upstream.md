# 调查总结：为什么 404 一直打上游，但 429/502 会停止？

## 🎯 你的问题

> 我发现上游429后，此网关打几次上游就会不打了，说是路由耗尽了，但是上游404啥的，就会一直打上游。
> 其实我希望429、502也能像404这种直接打上游，我想强制上游资源，别人也在抢占（不通过此项目网关）。

## ✅ 根因确认

### 404 为什么一直打上游？

**关键路径：**
1. `404` → `FailureClass::ProtocolUnsupported` (`upstream_feedback.rs:724`)
2. `ProtocolUnsupported.is_temporary()` = **false** (`state/types.rs:55-65`)
3. `terminal_failure()` 返回 `TerminalFailure::ProtocolUnsupported` (`route_attempts.rs:980-987`)
4. `round_terminal` 不是 `Temporary`，**不调用 `decide_with_reason()`** (`gateway.rs:9145`)
5. **直接 `break 'routing_rounds`**，404 立即返回给下游
6. **每个请求都是全新的第一轮，不进入重试循环**

**简单说：** 404 被分类为"非临时失败"，网关认为这是协议/端点问题，重试也不会好，所以直接返回，不消耗重试预算。

---

### 429 为什么会停止打上游？

**B3 门关闭（默认）：**
1. `429` → `RateLimited` → `is_temporary() = true`
2. 进入 `decide_with_reason()` (`gateway.rs:9145`)
3. **B3 门拦截**：`client_retryable_rate_limit && !rate_limit_internal_retry_enabled` (`route_retry.rs:280`)
4. 返回 `(None, None)`，**立即放弃**
5. 429 直接返回给下游（让 codex 客户端自己重试）

**B3 门打开：**
1. 通过 B3 门，进入重试循环
2. 路由被冷却（`DEFAULT_RATE_LIMIT_BASE = 30s`）
3. 下一轮 `reserve()` 返回 `Cooling`，**零物理尝试**
4. 等待 → 再试 → 再冷却 → **3 轮后报 `upstream_routes_exhausted`**

**简单说：** 429 被分类为"临时失败"，默认交给客户端重试（B3 门）。即使打开 B3 门，路由也会被冷却 30s，几轮后耗尽。

---

### 502 为什么会停止打上游？

1. `502` → `EdgeProxyError` → `is_temporary() = true` (`upstream_feedback.rs:740-752`)
2. `record_route_attempt()` 记录失败 (`gateway.rs:520-590`)
3. **路由被冷却**：`base=5s, max=60s, step 0→1→2` → `5s→10s→20s` (`route_health.rs:2335`)
4. 下一轮 `reserve()` 发现 `cooldown_until > now`，返回 `Cooling` (`route_health.rs:890-950`)
5. **候选列表跳过此路由，零物理尝试**
6. 等待恢复 → 再试 → 再冷却 → **3 轮后报 `upstream_routes_exhausted`**

**简单说：** 502 被分类为"临时失败"，网关认为上游/代理有问题，冷却 5s→10s 保护上游，几轮后耗尽。

---

## 🎯 解决方案

### 推荐方案：透传模式 + B3 门（零代码改动）

**配置（环境变量或 Admin 界面）：**

```bash
# 1. 关闭路由健康执行（透传模式）
UPSTREAM_ROUTE_HEALTH_ENFORCEMENT_ENABLED=false

# 2. 打开 B3 门（让 429 进入网关内重试）
UPSTREAM_RATE_LIMIT_INTERNAL_RETRY_ENABLED=true
```

**效果：**

| 错误码 | 当前行为 | 透传模式后 |
|-------|---------|-----------|
| **429** | B3 门拦截 → 立即返回 429 | reserve() 返回 Ready → **每次都打上游** → 返回上游的 429 |
| **502** | 冷却 5s→10s → 零物理尝试 → routes_exhausted | reserve() 返回 Ready → **每次都打上游** → 返回上游的 502 |
| **503** | 冷却 → 零物理尝试 → routes_exhausted | reserve() 返回 Ready → **每次都打上游** → 返回上游的 503 |
| **404** | **每次都打上游** | **保持不变** |

**核心原理：**

透传模式让 `reserve()` 永远返回 `Ready`，**完全绕过冷却检查**：

```rust
// src/state/route_health.rs:1040-1065
if !self.enforcement_enabled {
    // passthrough mode: record-only admission
    // 冷却路由 or 忙碌的 half-open lease never blocks the request
    return RouteAvailability::Ready(HealthLease { ... })
}
```

**这正是你需要的"强制打上游"模式。**

---

## 📊 为什么之前调的开关效果不明显？

你可能调过这些：

| 开关 | 效果 | 为什么不够 |
|-----|------|-----------|
| `upstream_transient_route_cooldown_base_seconds` | 减小冷却基数（5s → 2s） | 只是缓解，502 仍会被冷却，3 轮后还是耗尽 |
| `upstream_route_exhaustion_retry_max_rounds` | 调大重试轮次（3 → 10） | 429 被 B3 门拦截，压根不进入重试循环 |
| `upstream_route_exhaustion_retry_max_wait_ms` | 调大等待预算 | 429 被 B3 门拦截，不消耗等待预算 |

**根本问题：**
- 429 被 **B3 门直接拦截**（commit `54878119`，默认关闭）
- 502 被 **冷却机制阻断**（`reserve()` 返回 `Cooling`）

**只调这些参数无法绕过这两个机制。**

---

## 🔍 验证方法

### 1. 开启前观察（当前行为）

```bash
# 查看路由耗尽日志
grep "route_action.*routes_exhausted" logs/gateway.log | tail -10

# 应该看到：
# - give_up_reason: "round_cap" 或 "wait_budget"
# - routing_round: 2 或 3
# - cooldown_seconds: 5-20
```

### 2. 设置环境变量

**方式 1：直接设置**
```bash
export UPSTREAM_ROUTE_HEALTH_ENFORCEMENT_ENABLED=false
export UPSTREAM_RATE_LIMIT_INTERNAL_RETRY_ENABLED=true
```

**方式 2：Admin 界面**
- 访问 `http://your-gateway/admin/settings`
- 找到 `upstream_route_health_enforcement_enabled`，设为 `false`
- 找到 `upstream_rate_limit_internal_retry_enabled`，设为 `true`
- 点击 Save

### 3. 开启后观察（新行为）

```bash
# 查看路由行为
grep "route_action" logs/gateway.log | tail -30

# 应该看到：
# - routing_round=1（每个请求都是第一轮）
# - upstream_status=429 或 502（不再是 routes_exhausted）
# - 没有 cooldown_seconds（透传模式不冷却）
# - 没有 give_up_reason（不放弃）
```

### 4. 确认物理尝试

```bash
# 每个请求都应该有真实的上游响应
grep "upstream_status" logs/gateway.log | grep -E "429|502" | tail -10

# 应该看到每个请求都有 upstream_status（证明真实打了上游）
```

---

## 📝 部署建议（内网聚合网关）

根据 `DEPLOYMENT.md:185-300` 的内网部署指南，完整配置：

```bash
# === 透传模式（核心）===
UPSTREAM_ROUTE_HEALTH_ENFORCEMENT_ENABLED=false

# === B3 门（让 429 也进入重试）===
UPSTREAM_RATE_LIMIT_INTERNAL_RETRY_ENABLED=true

# === 可选：调整重试参数（虽然透传模式下不会耗尽）===
UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_ROUNDS=6
UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_WAIT_MS=60000

# === 可选：诊断增强 ===
UPSTREAM_ERROR_BODY_EXCERPT_ENABLED=true
UPSTREAM_ERROR_BODY_EXCERPT_MAX_BYTES=500
```

---

## ⚠️ 注意事项

### 透传模式的代价

1. **失去熔断保护**
   - 上游真的挂了，网关也会持续打
   - 不会自动隔离病态上游

2. **健康档案仍在记录**
   - Admin 界面能看到失败分布
   - 但不会阻断请求

3. **适用场景**
   - ✅ 内网单聚合网关
   - ✅ 上游资源竞争（别人也在抢）
   - ✅ 需要"打到上游给为止"的场景
   - ❌ 公网多租户网关（需要保护上游）

---

## 📚 相关 Commit

- `54878119` - B3 门开关（`upstream_rate_limit_internal_retry_enabled`）
- `1263cc42` - 透传模式（`upstream_route_health_enforcement_enabled`）
- `docs/superpowers/plans/2026-09-02-route-health-passthrough-switch.md` - 透传模式设计文档

---

## 🎉 总结

**问题：** 429/502 打几次就不打了，但 404 一直打上游

**根因：**
- 404 是"非临时失败"，不进入重试循环
- 429 被 B3 门拦截 或 被冷却阻断
- 502 被冷却阻断

**解决：**
```bash
UPSTREAM_ROUTE_HEALTH_ENFORCEMENT_ENABLED=false
UPSTREAM_RATE_LIMIT_INTERNAL_RETRY_ENABLED=true
```

**结果：** 429/502/503 都像 404 一样，每个请求都真实打上游。

**这就是你想要的"强制打上游"模式。零代码改动，配置即可启用。**
