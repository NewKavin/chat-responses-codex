# 解决方案设计：让 429/502 像 404 一样一直打上游

## 根因确认 ✅

### 为什么 404 一直打上游？
1. `404` → `FailureClass::ProtocolUnsupported`
2. `ProtocolUnsupported.is_temporary()` = **false**
3. `terminal_failure()` → `TerminalFailure::ProtocolUnsupported`
4. `round_terminal` 不是 `Temporary`，所以 **不调用 `decide_with_reason()`**
5. **直接 `break 'routing_rounds`**，返回 404 给下游
6. **每个请求都是全新的一轮，不进入重试循环**

### 为什么 429 会停止打上游？
**B3 门关闭时（默认）：**
1. `429` → `RateLimited` → `is_temporary() = true`
2. 进入 `decide_with_reason()`
3. B3 门拦截：`client_retryable_rate_limit && !rate_limit_internal_retry_enabled` → 返回 `(None, None)`
4. **直接放弃，429 返回给下游**

**B3 门打开时：**
1. 通过 B3 门，进入重试循环
2. 等待 `retry_after`（被 cap 限制）
3. 下一轮 `reserve()` 发现路由 `Cooling`，跳过
4. **3 轮后报 `upstream_routes_exhausted`**

### 为什么 502 会停止打上游？
1. `502` → `EdgeProxyError` → `is_temporary() = true`
2. `record_route_attempt()` 记录失败 → **路由被冷却 5s→10s**
3. 下一轮 `reserve()` 发现 `cooldown_until > now` → 返回 `Cooling`
4. **候选列表跳过此路由，零物理尝试**
5. **3 轮后报 `upstream_routes_exhausted`**

---

## 方案对比

### 方案 A：使用现有的透传模式（推荐 ⭐⭐⭐⭐⭐）

**原理：** 已有的 `upstream_route_health_enforcement_enabled = false`

**实现位置：** `src/state/route_health.rs:1040-1065`

```rust
if !self.enforcement_enabled {
    // passthrough mode: record-only admission
    // 冷却路由 or 忙碌的 half-open lease never blocks the request
    return RouteAvailability::Ready(HealthLease { ... })
}
```

**效果：**
- ✅ 502/503/429 **不再被冷却阻断**
- ✅ `reserve()` 永远返回 `Ready`
- ✅ **每个请求都真实打上游**
- ✅ 上游错误原样透传给下游
- ✅ 健康档案仍在记录（管理界面能看到失败分布）
- ⚠️ 但 429 仍会进入 `decide` → B3 门拦截（需要组合方案）

**配置：**
```bash
UPSTREAM_ROUTE_HEALTH_ENFORCEMENT_ENABLED=false
UPSTREAM_RATE_LIMIT_INTERNAL_RETRY_ENABLED=true  # 打开 B3 门
```

**优点：**
- ✅ **零代码改动**（功能已存在，commit `1263cc42`）
- ✅ 专为内网聚合网关设计
- ✅ 测试覆盖完整
- ✅ 配置即可启用

**缺点：**
- ⚠️ 失去熔断保护（上游真挂了也会持续打）
- ⚠️ 429 仍需打开 B3 门才能进入重试（但透传模式下重试是多余的）

**适用场景：**
- ✅ 内网单聚合网关
- ✅ 上游资源竞争（别人也在抢）
- ✅ 需要"强制打上游"的场景

---

### 方案 B：打开 B3 门（部分解决）

**原理：** `upstream_rate_limit_internal_retry_enabled = true`

**实现位置：** `src/server/gateway/route_retry.rs:270-289`

```rust
if client_retryable_rate_limit && !self.rate_limit_internal_retry_enabled {
    return (None, None);  // B3 门关闭时拦截
}
// B3 门打开后，429 进入正常重试循环
```

**效果：**
- ✅ 429 **不再被 B3 门拦截**
- ✅ 429 进入重试循环，等待后再次打上游
- ⚠️ 但仍受 `max_rounds=3` 限制
- ❌ 502 仍会被冷却阻断

**配置：**
```bash
UPSTREAM_RATE_LIMIT_INTERNAL_RETRY_ENABLED=true
```

**优点：**
- ✅ **零代码改动**（功能已存在，commit `54878119`）
- ✅ 专为 429 设计

**缺点：**
- ❌ 只解决 429，不解决 502
- ⚠️ 仍受 `max_rounds` 限制，3 轮后还是会报 `routes_exhausted`

---

### 方案 C：修改 is_temporary()（不推荐 ❌）

**原理：** 让 429/502 也像 404 一样，`is_temporary() = false`

**实现：**
```rust
// src/state/types.rs
pub fn is_temporary(self) -> bool {
    matches!(
        self,
        Self::CapacityUnavailable
            | Self::ConcurrencySaturated
            | Self::TransientServer
            | Self::Transport
            // 移除 RateLimited 和 EdgeProxyError
            // | Self::RateLimited
            // | Self::EdgeProxyError
    )
}
```

**效果：**
- ✅ 429/502 不进入 `Temporary` 分支
- ✅ 不进入重试循环
- ❌ 破坏 OpenAI 协议语义（429 必须带 `Retry-After`）
- ❌ 需要新建 `TerminalFailure` 变体
- ❌ 影响所有使用 `is_temporary()` 的地方

**优点：**
- 无

**缺点：**
- ❌ 破坏协议语义
- ❌ 需要大量代码改动
- ❌ 影响现有行为
- ❌ 需要重新测试所有路径

---

## 推荐方案：A + B 组合

### 配置（最简单）

```bash
# 1. 关闭路由健康执行（透传模式）
UPSTREAM_ROUTE_HEALTH_ENFORCEMENT_ENABLED=false

# 2. 打开 B3 门（让 429 也进入重试，虽然透传模式下是多余的）
UPSTREAM_RATE_LIMIT_INTERNAL_RETRY_ENABLED=true

# 3. 可选：调大重试轮次（虽然透传模式下不会耗尽）
UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_ROUNDS=10
```

### 效果

| 错误码 | 行为 |
|-------|------|
| **429** | 进入 decide → B3 门通过 → 但 reserve() 返回 Ready（透传）→ **每次都打上游** |
| **502** | reserve() 返回 Ready（透传）→ **每次都打上游** |
| **503** | reserve() 返回 Ready（透传）→ **每次都打上游** |
| **404** | 保持原样，**每次都打上游** |

**最终结果：**
- ✅ **所有失败都真实打上游**
- ✅ **上游错误原样返回给下游**
- ✅ **零代码改动**
- ✅ **配置即可启用**

### 验证方法

1. **开启前观察：**
   ```bash
   # 查看日志
   grep "route_action.*routes_exhausted" logs/gateway.log | tail -5
   # 应该看到 429/502 → routes_exhausted
   ```

2. **设置环境变量：**
   ```bash
   UPSTREAM_ROUTE_HEALTH_ENFORCEMENT_ENABLED=false
   UPSTREAM_RATE_LIMIT_INTERNAL_RETRY_ENABLED=true
   ```

3. **开启后观察：**
   ```bash
   # 查看日志
   grep "route_action" logs/gateway.log | tail -20
   # 应该看到：
   # - 429 → 下游收到 429（不再是 routes_exhausted）
   # - 502 → 下游收到 502（不再是 routes_exhausted）
   # - routing_round=1（每个请求都是第一轮）
   ```

4. **确认物理尝试：**
   ```bash
   # 每个请求都应该有 upstream_status
   grep "upstream_status" logs/gateway.log | tail -10
   ```

---

## 总结

**你的需求：** "429、502 也能像 404 一样直接打上游，强制抢占上游资源"

**最佳方案：** 使用现有的透传模式 + B3 门

**零代码改动，两行配置：**
```bash
UPSTREAM_ROUTE_HEALTH_ENFORCEMENT_ENABLED=false
UPSTREAM_RATE_LIMIT_INTERNAL_RETRY_ENABLED=true
```

**这就是你想要的行为。**
