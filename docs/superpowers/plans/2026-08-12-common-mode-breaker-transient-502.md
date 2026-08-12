# 方案：共模熔断（common-mode breaker）对瞬态 502 误判——内网聚合网关场景修复

日期：2026-08-12
状态：✅ 已完成（2026-08-12）
- 设置层：`3c6233b`（新阈值与开关接入 runtime settings / 管理页 / 前端目录）
- 任务 1+2（类别拆分 + host 多样性 + 延迟重放 + 新错误码）：`b73f173`
- 任务 3（同路由快速重试预算化 + 双开关）：`c630e97`
- 测试（任务 1-6 全部 7 类用例）：`6a7474b`
- 任务 4 Dashboard 单列：`95f551a`
- 任务 5 部署文档：`da89bd8`
关联报错：
```
upstream rejected this request on multiple routes with the same failure
(transient_server consecutive similar failures (upstream HTTP 502));
the request was not replayed across the remaining routes.
First upstream error: upstream server error (status 502)
```

---

## 一、报错来源（已核实代码）

该错误由 B2 共模熔断产生，完整链路：

1. 上游返回 502。若响应体是 JSON 且不含请求拒绝类关键词 → 分类为 `FailureClass::TransientServer`（`src/upstream_feedback.rs:430-471`）；若是 HTML/空体 → `EdgeProxyError`（`upstream_feedback.rs:409-428`）。两者都在熔断类别里（`src/server/gateway.rs:735-742` `is_common_mode_breaker_class`）。
2. 单个下游请求的 failover 过程中，**不同路由**（RouteHealthKey 含 key 指纹，同一上游的两把 key 也算两条路由）连续以**完全相同的 (class, upstream_status)** 失败时，streak +1（`gateway.rs:6874-6947`）。
3. streak ≥ `upstream_common_mode_breaker_threshold`（默认 **2**，`src/state/types.rs:105`）→ 熔断触发：停止剩余路由重放（`break 'routing_rounds`）、回滚本请求写入的冷却、直接向下游返回 502 `upstream_request_shape_rejected`，即用户看到的报错（`gateway.rs:749-783`）。
4. 关键副作用：熔断短路发生在 routing_rounds 循环的**临时故障恢复机制之前**——正常情况下全路由瞬态失败会走 `earliest_temporary_route_recovery` 等待+重试轮（`gateway.rs:6968-6974`，轮数受 `UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_ROUNDS` 限制），熔断把这条自愈路径也切断了。

## 二、根因

**熔断的前提假设在内网部署下不成立。**

设计假设（见 `gateway.rs:732-734` 注释）："不同路由重复出现完全相同的失败 ⇒ 请求形状（request shape）有问题，是请求自身的错，继续重放只会烧掉整个池"。这对 `RequestRejected`（400 语义）成立；但对 `TransientServer` 502 不成立：

1. **内网路由共享同一基础设施**。典型内网部署里，所有"不同路由"其实穿过同一个出口代理 / 同一个 one-api·new-api 聚合网关，甚至就是**同一上游 host 上的多把 key**。共享跳点发生瞬时 502（重启、连接池抖动、后端瞬断）时，连续两条路由必然拿到一模一样的 (transient_server, 502) —— 这是基础设施共模瞬断，不是请求形状问题。
2. **默认阈值 2 过于激进**。同一上游配两把 key 的最小部署，一次网关抖动就凑满 streak=2。
3. **误判后果是硬失败**：请求不再重放、不等待恢复、立刻 502 返回下游；而实际上换一条路由或等 1 秒重试大概率成功。错误码 `upstream_request_shape_rejected` 还会误导排查方向。

结论：502 场景下熔断把"应该重试的瞬态故障"当成了"不该重试的请求缺陷"。

## 三、立即缓解（不改代码，内网可先做）

- 管理设置或环境变量把 `UPSTREAM_COMMON_MODE_BREAKER_THRESHOLD=0`（0=禁用，`gateway.rs:6876` 有 `threshold > 0` 守卫，测试 `0dfe985` 已覆盖）。禁用后 502 走正常逐路由 failover + 临时恢复等待轮，自愈能力恢复。
- 代价：真正的请求形状问题会把各路由烧进短冷却。内网单聚合网关场景下可接受，待下述开发完成后再恢复启用。

## 四、开发任务

### 任务 1：按失败类别拆分熔断语义（核心）

`is_common_mode_breaker_class` 与 trip 行为按类别区分：

1. `RequestRejected`：保持现状（停止重放 + 400/502 返回），假设成立。
2. `TransientServer` / `EdgeProxyError`：streak 达阈值后**不再直接返回错误**，改判为「共模瞬态」：
   - 在请求预算内做**一次延迟重放轮**：sleep min(500ms, 剩余预算)，重放尚未尝试过的路由（或重置为整轮）；
   - 延迟重放仍以相同签名失败 → 返回 502，错误码改为 `upstream_transient_pool_failure`，消息面向操作者（见任务 4），并带 `Retry-After`；
   - 保留现有"回滚本请求写入的冷却"行为（共模瞬态同样不应烧路由）。
3. 为 transient 类引入独立运行时阈值 `upstream_common_mode_transient_threshold`（默认 4，0=禁用该类熔断），`RequestRejected` 沿用现有 `upstream_common_mode_breaker_threshold`。两者都接入 runtime settings 校验（≤64）与管理设置页。

### 任务 2：streak 计数要求基础设施多样性

1. streak 只在**不同上游 host** 的路由失败时增长：从 `upstream.base_url` 提取 host，与上一条失败路由的 host 相同（同一聚合网关/同一上游的不同 key）→ 视为路由局部故障，streak 重置为 1（沿用现有同 route 重置分支的语义，`gateway.rs:6878-6891`）。
2. 实现方式：common_mode 状态元组里把 `RouteHealthKey` 换成/补充 `(RouteHealthKey, host)`；host 提取用现有 URL 解析工具（`join_upstream_url` 同源的 parse 逻辑），解析失败时退回按 route 计数。
3. 效果：内网"一个 new-api 网关 + N 把 key"的部署形态下，共模熔断天然不会因该网关瞬断而触发；只有真正跨 host 的一致失败才计入。

### 任务 3：瞬态 5xx 的同路由快速重试（一次）

1. 路由失败分类为 `TransientServer` 且状态 502/503/504 时，在进入 failover 前对**同一路由**做一次快速重试：退避 200–500ms，尊重上游 `Retry-After`（上限 2s），受请求总预算约束。
2. 仅对**尚未向下游发出任何字节**的请求生效；流式请求需与现有 stream-only recovery / mid-stream failover 机制（参考提交 `368664c`）兼容——已发流的场景维持现状不重试。
3. 重试成功 → 不记失败、不进 streak；重试仍失败 → 按现有路径记失败并进入 failover。避免单次网络毛刺被放大为路由失败乃至熔断证据。
4. 加开关：runtime setting `upstream_transient_same_route_retry_enabled`（默认 true）。

### 任务 4：错误信息与可观测性

1. 共模瞬态错误消息改写，指明疑似共享网关瞬断而非请求问题，例如：
   `multiple routes failed with identical transient upstream errors (HTTP 502) — likely a shared upstream gateway outage; retried once after backoff. First upstream error: ...`
2. details 增加：`failed_route_count`、`distinct_hosts`、`streak`、`threshold`、`retried: bool`；沿用现有 `common_mode: true` 字段。
3. tracing::warn 的 trip 日志（`gateway.rs:6913-6927`）补充 distinct_hosts 与判定分支（request_shape / transient）。
4. Dashboard 失败分类统计（`src/server/admin.rs:478` `classify_dashboard_failure`）把共模 trip 单列一类，管理页可见次数趋势。

### 任务 5：文档

`docs/` 部署文档新增「内网/聚合网关部署建议」小节：
- 两个阈值与重试开关的含义、默认值、推荐值（单聚合网关：transient 阈值 0 或 ≥4）；
- 报错 `upstream_transient_pool_failure` / `upstream_request_shape_rejected` 的排查指引。

## 五、测试要求

复用 `tests/` 现有 mock 上游设施（参考 `tests/feedback.rs` 共模熔断既有用例、提交 `0dfe985` 的阈值 0 用例）：

1. 两条**同 host** 路由连续 502 → 不触发共模熔断，走正常 failover/恢复（断言最终行为与熔断禁用时一致）。
2. 两条**不同 host** 路由连续 502，transient 阈值 2 → 触发共模瞬态：先延迟重放一轮；mock 重放成功 → 请求成功返回；mock 重放仍 502 → 返回 502 `upstream_transient_pool_failure` 且带 Retry-After、`common_mode:true`、`retried:true`。
3. `RequestRejected` 复读 → 行为不变（立即停止重放，`upstream_request_rejected`），回归既有用例。
4. 同路由 502 一次后重试成功 → 无失败记录、无冷却写入；重试仍 502 → 记一次失败。
5. 流式已发字节场景 → 不做同路由重试（断言不重复向上游发请求）。
6. 阈值 0 → 两类熔断均禁用（回归 `0dfe985`）。
7. runtime settings 校验：新阈值 >64 拒绝；管理设置页序列化往返。
