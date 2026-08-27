# 上游并发槽位记账可信化 + 上游账号字段批量修改

- 日期：2026-08-27
- 状态：待开发（本文档用于交接给其他模型实现）
- 目标：**彻底**解决内网出现的「上游没到限流、请求也没打到上游，网关自己返回 429 `upstream_routes_exhausted`」问题，并补上运维必需的「上游账号字段批量修改」能力。
- 前置约束（用户现场事实，均已确认）：
  1. **上游账号 key 本身只允许 4 并发**。因此"把 `max_concurrency` 调大"不是可用解法，方案必须让 **槽位记账本身可信**，在 4 并发的硬上限下把请求排好队服务掉。
  2. **内网聚合网关是按 key 限流的**（不是按账号/IP 统一限流）。所以总容量 = 有效 key 数 × 4，是真实可用容量，不是幻觉；网关侧 `AccountConcurrencyKey::new(upstream_id, key_fingerprint)`（`src/state.rs:3625-3660`）的记账维度与上游真实限流维度**完全一致**，`max_concurrency = 4` 是正确的出厂值，**不要改它**。本方案要修的是记账的可靠性，不是记账的维度。
  3. 现场模型分布：glm5.2 只有 **1 个**上游 key（容量 4），deepseek 有 **7 个**上游 key（容量 28），且要聚合到**同一个下游 key** 对外提供——这是 C7 存在的原因。

---

## 1. 报错来源

现场（内网单聚合网关部署）返回：

```
HTTP 429
error.code = upstream_routes_exhausted
all eligible upstream routes are temporarily unavailable：
  upstream concurrency limit saturated（1 route，upstream=xxx）；
  please try again in 1s；
  gateway already retried for 32.1s across 6 routing rounds
```

三个与直觉矛盾的点，逐个已在代码里定位：

| 现象 | 结论 |
| --- | --- |
| 上游限制没达到 | 对的——**这个 429 不是上游发的**，是网关自己 pre-dispatch 的本地租约闸门发的 |
| 请求没打到上游 | 对的——闸门在发起 HTTP 之前就 return 了，**一次上游请求都没发出** |
| 6 轮 > `max_rounds=3` | 不是配置被人改过：`ConcurrencySaturated` 走的是**独立的并发预算** 32 轮 / 30s |

---

## 2. 根因

### 2.1 429 的真实出处：本地租约闸门（不是上游）

`src/state.rs:3681-3689`：

```rust
if state.active_leases.get(&account).map_or(0, HashMap::len)
    >= upstream.max_concurrency.max(1) as usize
{
    return Err(UpstreamAdmissionError::new(
        UpstreamAdmissionRejectionReason::LocalConcurrency,
        "upstream request concurrency capacity is full".into(),
        1,                       // ← 硬编码 Retry-After = 1s
    ));
}
```

- 槽位按 `AccountConcurrencyKey::new(upstream_id, key_fingerprint)` 计（`src/state.rs:3625-3660`），即 **每个上游 key 一份预算**；
- `upstream.max_concurrency` 默认 4（`src/state/types.rs:1154` `default_upstream_max_concurrency()`）——**恰好等于用户 key 的真实并发上限 4**，所以这个闸门在现场是天天会撞到的常态路径，不是边缘情况；
- 错误类归一链：`UpstreamAdmissionRejectionReason::LocalConcurrency` → `FailureClass::ConcurrencySaturated`（`src/server/gateway.rs:6805-6816`）→ `GatewayError::ConcurrencyFull`（`src/server/gateway/errors.rs:605`）→ **HTTP 429**（`src/server/gateway/errors.rs:512-514`）。

**结论：429 语义被污染了。** 客户端（codex 等）看到 429 会当成"上游在限流我"，按 Retry-After 退避；实际是网关自限流，且 Retry-After 是硬编码的 1s，与真实腾出槽位的时间毫无关系。

### 2.2 槽位为什么会被"占着不放"：释放路径全是软的

槽位只有两种减少方式：**显式释放** 和 **TTL 过期后的懒清扫**。两条都不可靠：

**(a) 显式释放挂在 `runtime.spawn` 上，且 JoinHandle 被丢弃** —— `src/server/gateway.rs:3263-3269`：

```rust
impl Drop for UpstreamRequestGuardInner {
    fn drop(&mut self) {
        if let Ok(Some(task)) = self.spawn_release() {
            drop(task);          // 分离任务：没人等它，没人知道它有没有跑
        }
    }
}
```

`spawn_release()`（`src/server/gateway.rs:3210-3260`）在拿到 runtime handle 后 `runtime.spawn(async move { state.release_upstream_request(lease).await })`。**runtime 正在 shutdown 时，spawn 出来但还没被 poll 的任务会被直接丢弃而不执行** → 租约永不释放。

**(b) 释放失败后不可重试** —— 同上函数：

```rust
if let Err(error) = &result { tracing::error!(...); } else { release_guard.complete(); }
```

`release_guard.complete()` 只在成功时调用。失败时 `release_state` 永久停在 `RELEASING`（`src/state.rs:440-480` 的 `LEASE_RELEASE_ACTIVE/RELEASING/RELEASED` 三态 + `impl Drop for LeaseReleaseGuard`），而 `GatewayReleaseGuard::acquire` 在非 ACTIVE 时返回 `Ok(None)` —— **后续任何一次 drop 都无法再发起释放**。

**(c) 同步兜底会静默失败** —— `expire_upstream_request_lease_sync`（`src/state.rs:3956-3971`）用 `try_lock()`，拿不到锁直接 `return false`；调用方（在 `Drop` 里，无 async 上下文）除了打一条 `tracing::error!("...lease left for TTL reclamation")` 之外无事可做。

**(d) guard 是 `Arc` 克隆，任何一份被长期持有就锁死槽位** —— `UpstreamRequestGuard` 是 `Clone`（`src/server/gateway.rs:3270-3273`，内部 `Arc<UpstreamRequestGuardInner>`），并且真的被克隆进了 `StreamCompletionContext`（`src/server/gateway.rs:6855`）。释放发生在**最后一份**克隆 drop 时；任何一处泄漏都表现为槽位被永久占用。

**(e) 于是 TTL 成了唯一的真实回收机制，而 TTL 默认 3600s** —— `src/state/types.rs:215` `DEFAULT_UPSTREAM_LOCAL_LEASE_TTL_SECONDS = 3600`（range `60..=86400`）。

> **这就是事故的完整机理：4 个泄漏租约把这个 key 钉死整整 1 小时。** 因为 `max_concurrency` 默认 4 == key 真实上限 4，只要泄漏 4 次，该账号在一小时内**每一个**新请求都会被本地闸门拒掉，且一次上游请求都发不出去。

### 2.3 续约只覆盖流式，所以"直接调小 TTL"是个陷阱

`UpstreamRequestReservation`（`src/server/gateway.rs:3307-3345`）的文档注释写得很清楚：

> Long streaming requests renew their local/Redis upstream lease at half the configured TTL so the slot is never reclaimed mid-stream; leaked guards (dropped without release) stop producing chunks and therefore stop renewing, letting the TTL lapse and the lazy sweep reclaim the slot.

`renew_if_due` 的调用点只有 4 处，全在 `src/server/gateway/stream.rs:805,809,1607,1611`，**由"收到 chunk"驱动**。因此：

- **非流式请求完全不续约**。TTL 若从 3600 降到比如 120s，一个 5 分钟的长推理非流式请求会在中途被清扫掉租约 → 第 5 个请求被放行 → **对只允许 4 并发的上游真正超发**，把网关自限流问题换成上游 429 问题；
- 流式请求在**长时间静默**（推理期不吐 chunk）时也不续约，同样有被误清扫的风险。

**所以 TTL 必须先有"与 chunk 无关的心跳"，才能安全下调。**

### 2.4 已有的排队机制对这条失败路径是死代码

代码里已经有一整套 per-account 排队/探测设施：`src/state/account_concurrency.rs`（`AccountConcurrencyRegistry`、`register_waiter`、`register_waiter_if_saturated`、`try_probe`、`AccountWaitTicket`，620 行）+ Redis 侧 `src/state/redis_runtime/account_waiter.lua`，AppState 包装在 `src/state.rs:1255-1315`。

但是：

- `register_waiter_if_saturated` 只在 `state.saturated == true` 时发票（`src/state/account_concurrency.rs:270-273`）；
- `saturated` 只由 `observe_account_concurrency` 置位（`src/state.rs:1265-1266`）；
- `observe_account_concurrency` 的**唯一**调用点是 `src/server/gateway/account_recovery.rs:355`，条件是 `AccountProbeOutcome::ConcurrencyRejected`——那是**上游真的回了 429** 之后的账号恢复流程；
- 本地闸门拒绝那条路（`src/server/gateway.rs:6766-6825`）**从头到尾没有调用 `observe_account_concurrency`**，末尾直接 `break`。

**结论：本地闸门饱和时，账号永远不会被标记 saturated，排队机制永远不会被触发。** 现成的队列在这条路上是死代码——请求不是"排队等槽位"，而是"被拒绝 → 冷却 → 下一轮再撞 → 再拒绝"。

### 2.5 「6 轮 / 32.1s」的解释：并发有独立且巨大的预算

`src/server/gateway/route_retry.rs:305-315`：

```rust
let concurrency_recovery = health_recovery.is_some_and(|r| r.class == ConcurrencySaturated);
let (max_wait, max_rounds) = if concurrency_recovery {
    (self.concurrency_max_wait, self.concurrency_max_rounds)   // ← 并发专用
} else {
    (self.max_wait, self.max_rounds)                            // ← 3 轮
};
```

默认值（`src/state/types.rs:261-262`）：

```rust
pub const DEFAULT_UPSTREAM_CONCURRENCY_RECOVERY_MAX_WAIT_MS: u64 = 30_000;
pub const DEFAULT_UPSTREAM_CONCURRENCY_RECOVERY_MAX_ROUNDS: u32 = 32;
```

**"6 routing rounds"没有超配额**，它离 32 轮还很远；真正终止请求的是 30s 的 `concurrency_max_wait`（报文里 32.1s = 30s 预算 + 抖动/最后一轮开销）。此外半开忙等有自己的 10 轮预算（`DEFAULT_UPSTREAM_ROUTE_HALF_OPEN_BUSY_MAX_ROUNDS = 10`，`src/state/types.rs:206`；`route_retry.rs:70-79` 明确 busy 等待不推进 `current_round`），也会累加到 `waited` 却不计入 rounds。

每轮的等待时长来自 `ConcurrencySaturated` 的冷却：`src/state/route_health.rs:1417` 对该类**故意让 upstream 的 explicit Retry-After 直接取胜**（不做 `explicit.max(local)`），而本地闸门给的 explicit 就是**硬编码的 1s**（§2.1）——于是探测延迟序列 `DEFAULT_UPSTREAM_CONCURRENCY_PROBE_DELAYS_MS = [100,200,400,800,1000,2000]`（`src/state/types.rs:263-264`，经 `route_cooldown_with_concurrency_delays`，`src/state/route_health.rs:2033-2034`）被这个 1s 常量旁路掉了。

> 逐轮时长的精确分解（哪几轮是 busy 等、哪几轮是 1s 冷却）需要现场日志确认，本文不做断言；但**"rounds 6 > 3 不代表配置被改过"**这一条是代码层确定的。

### 2.6 等待在这里是纯浪费

冷却 + 重试的前提是"等一会儿情况会变好"。本地闸门场景下：

- 槽位由**活着的并发请求**持有 → 等待是合理的，它们会结束；
- 槽位由**泄漏租约**持有 → 等待毫无意义，它只会在 TTL（3600s）到点才变，30s 预算 100% 是浪费。

**网关今天分不清这两种情况**——`active_leases` 里只存 `lease_id -> 过期时刻`（`src/state.rs:6434`），没有"上次心跳时间"，所以无法判断持有者是否还活着。这是"要么白等 30s、要么误杀在途请求"两难的根源，也是**必须补的核心数据**。

### 2.7 运维缺口

- `GET /api/admin/upstreams` 已经暴露 `in_flight` 与 `leaked_reclaimed_total`（`src/server/admin.rs:570-576` → `state.upstream_runtime_snapshots()`；`in_flight` = `active_upstream_lease_count()`，`src/state.rs:6464-6472`，只数**未过期**租约）。**现场可立刻据此确诊，不需要改代码**：`in_flight == max_concurrency` 而实际无请求在跑 ⇒ 泄漏；`leaked_reclaimed_total > 0` ⇒ 已经发生过泄漏（仅本地后端，Redis 后端恒 0）。
- **没有任何接口能强制释放/重置租约**。管理路由只有 `/api/admin/upstreams/{id}/route-health/reset`（`src/server/gateway.rs:2320`），没有 concurrency reset。现场唯一处置手段是**重启网关**。
- **没有批量改字段的接口**。已有 `/upstreams/batch`（批量创建，`admin_create_upstreams_batch` `src/server/admin.rs:1416`）、`/batch-toggle`（`:2390`）、`/batch-delete`（`:2424`），**唯独缺"批量改字段"**——而修这次事故恰恰需要一次性给几十个账号调 `max_concurrency` / TTL。

---

## 3. 开发任务

> 顺序即优先级。C1 是"彻底"的关键（让记账不再可能错），C2/C3 让 4 并发硬上限下的请求能排队服务掉，C4 让失败可解释，C5/C6 是运维闭环。
> 每个任务都要能独立编译通过、独立跑测试通过，便于分批提交。

### C1 — 让槽位记账可信：本地后端同步、无条件释放

**C1.1 把租约表从 `upstream_runtime_state` 拆出来，改用同步锁**

- 新增 `struct UpstreamLeaseTable`，字段：`HashMap<AccountConcurrencyKey, HashMap<String, LeaseRecord>>`；
- `struct LeaseRecord { expires_at: tokio::time::Instant, last_renewed_at: tokio::time::Instant, kind: LeaseKind /* Streaming | Unary */ }`（`last_renewed_at` 是 §2.6 要的核心新数据）；
- 存放在 `AppState` 的 `Arc<std::sync::Mutex<UpstreamLeaseTable>>`，**禁止跨 `await` 持锁**；
- 先例可循：`AccountConcurrencyRegistry` 本身就是 `std::sync::Mutex`（`src/state/account_concurrency.rs:227` `self.inner.lock().expect("account registry lock poisoned")`），本项目已接受这个模式；
- 迁移 `state.active_leases` 的**全部**读写点，一个都不能漏：
  - `try_reserve_upstream_account_request`（`src/state.rs:3681-3700`）—— 普通请求闸门；
  - `try_reserve_upstream_account_hedge`（`src/state.rs:3721` 起）里的**两处**闸门：`:3767`（"upstream hedge concurrency capacity is full"）与 `:3797`（"upstream request concurrency capacity is full"）；
  - `release_upstream_request`（`:3888-3915`）、`expire_upstream_request_lease_sync`（`:3956-3971`）、`prune_expired_upstream_leases`（`:6449-6460`）、`active_upstream_lease_count`（`:6464-6472`）；
- **注意 hedge 路径**：一共 **3 处**同样的 `active_leases.len() >= max_concurrency` 判断、**3 处**同样的硬编码 `retry_after = 1`。只修普通请求那一处的话，hedge 路径仍然会以旧方式咬住槽位并返回假的 1s。同时提醒：hedge 请求消耗的是**同一份** `max_concurrency` 预算——在只有 4 并发的账号上，开 hedge 会直接吃掉真实槽位，方案落地后要重新评估 hedge 在该部署下是否值得开。
- **注意**：配额事件（`minute_events` / `five_hour_events`）留在 `upstream_runtime_state` 不动，只搬租约。目的是把"必须能在 `Drop` 里同步完成的操作"与"可以 async 的操作"彻底分开。

**C1.2 `Drop` 里对本地后端同步释放，不再依赖 `runtime.spawn`**

- 改 `UpstreamRequestGuardInner::spawn_release`（`src/server/gateway.rs:3210-3260`）：
  - 本地后端（`RuntimeCoordinationBackend::Local`）→ **直接同步 `lock()` + `remove()`**，无 spawn、无 `try_lock`、无 TTL 依赖；
  - Redis 后端 → 仍然 spawn（Redis 调用必须 async），并保留 Redis 自身 TTL 兜底；
- `impl Drop`（`:3263-3269`）不再 `drop(task)` 了事：本地路径必须在 `drop` 返回前完成释放。

**C1.3 释放失败必须可重试**

- `GatewayReleaseGuard` / `LeaseReleaseGuard`（`src/state.rs:440-480`）：release 返回 `Err` 时把状态**回滚为 `ACTIVE`**，而不是留在 `RELEASING`；
- 这样最后一次 `Drop`（或 C2.3 的陈旧扫描）还能再试一次。

**C1.4 删掉 `try_lock` 静默失败**

- C1.1 之后 `expire_upstream_request_lease_sync` 可以直接 `lock()`，删除"拿不到锁就 `return false`"的分支和随之而来的 `"lease left for TTL reclamation"` 错误日志路径。

### C2 — TTL 降级为兜底，并为所有在途请求提供心跳

**C2.1 心跳覆盖非流式（先做这个，再改 TTL）**

- `UpstreamRequestReservation::new`（`src/server/gateway.rs:3307-3345`）启动一个续约任务：间隔 `ttl/3`（下限 1s），`guard` drop 时通过 `AbortHandle` abort；
- 保留 `stream.rs` 现有 per-chunk `renew_if_due` 作为双保险（它已有 `ttl/2` 节流，不会打架）；
- 续约时同时更新 `LeaseRecord::last_renewed_at`。

**C2.2 下调 TTL 默认值**

- `DEFAULT_UPSTREAM_LOCAL_LEASE_TTL_SECONDS`：`3600` → `300`（`src/state/types.rs:215`）；
- 加**编译期不变量**（仿 `src/state/types.rs:169` 已有的 `const _: () = { assert!(...) }` 写法）：TTL 必须 ≥ `3 × 心跳间隔`，防止把 TTL 配到心跳来不及续的区间；
- `src/state/runtime_settings.rs` 的校验/修复函数同步收敛（参考已有 `repair_cooldown_ceiling_invariant` 的做法），运行时改小 TTL 时不能破坏该不变量。

**C2.3 陈旧租约立即回收，不等 TTL**

- 新增 `upstream_lease_stale_after_ms`（默认 = `2 × 心跳间隔`）；
- `prune_expired_upstream_leases` 除了 `expires_at` 过期，也回收 `now - last_renewed_at > stale_after` 的租约，并单独计数（不要混进 `leaked_reclaimed_total`，见 C5.1）；
- 这才是"泄漏后最多几十秒自愈"而不是"最多 1 小时自愈"。

### C3 — 饱和时排队，而不是"拒绝 + 轮询"

**C3.1 本地闸门拒绝时标记 saturated（打通死代码）**

- 在 `src/server/gateway.rs:6766-6825` 的 `LocalConcurrency` 分支里调用 `state.observe_account_concurrency(&account_key, retry_after)`；
- 这一步是 §2.4 全部后续能力的开关。

**C3.2 拿票排队等槽位**

- 拒绝路径改为先 `state.register_account_waiter_if_saturated(...)`（`src/state.rs:1290-1315`）拿 `AccountWaitTicket`；
- 释放槽位时（C1.2 的同步释放点）`notify` 等待者，FIFO 按 `registered_at_ms`；
- 拿到通知后重试 `try_reserve_upstream_account_request`，成功即正常发请求；
- 取消/超时走已有 `cancel_waiter`（`src/state/account_concurrency.rs:300`）；
- **回归风险提示**：已有测试 `chat::rate_limits::cancelled_account_waiter_does_not_block_the_next_request` 覆盖取消语义，且它在满并行负载下本来就偶发（本次改动前已确认是既有竞争，隔离跑 rc=0）——**不要把它当成本次引入的回归去"修"**，但要确认改动后它在隔离下仍然通过。

**C3.3 队列必须有界**

- 新增 `upstream_account_queue_max_depth`（建议默认 `max_concurrency × 4`）、`upstream_account_queue_max_wait_ms`（建议默认 10_000）；
- 超过深度或超时才失败，失败走 C4.2 的新错误码。

**C3.4 Retry-After 说真话**

- 删掉**三处**硬编码 `1`（`src/state.rs:3688`、`:3774`、`:3804`）；
- 改为估算：`max(最老活跃租约的剩余存活时间, 探测序列下一档)`，并按队列位置放大；
- `ConcurrencySaturated` 在 `src/state/route_health.rs:1417` 让 explicit 直接取胜——这个设计**保留**（注释里说明了理由：并发的 Retry-After 是真实槽位信息），但前提是喂进去的值必须真实，C3.4 正是补这个前提。

### C4 — 本地闸门失败要快速失败，且客户端可区分

**C4.1 纯本地拒绝不再烧 30s / 32 轮**

- 判据：本轮 `physical_attempt_count == 0`（一次上游请求都没发出）且全部候选都是 `LocalConcurrency` 拒绝；
- 新增 `upstream_local_gate_max_wait_ms`（建议默认 3_000），该场景下用它代替 `concurrency_max_wait`（30s）；
- 排队（C3）成功时不受此限——**排队等待和盲目重试要分开计时**：前者有明确的"槽位会释放"证据，后者没有。

**C4.2 新错误码，让网关自限流可被区分**

- HTTP 状态码**保持 429**（兼容性）；
- `error.code` 从 `upstream_routes_exhausted` 改为 `gateway_concurrency_saturated`；
- `details` 带：`in_flight`、`max_concurrency`、`stale_lease_count`、`queue_depth`、`queue_position`、`physical_attempt_count: 0`、`retry_after_source`；
- 文案明确"这是网关侧并发闸门，不是上游限流"，避免运维再次误判。

**C4.3 补文档，防止下次再被 rounds 数字误导**

- 在 `docs/` 说明：`ConcurrencySaturated` 走 32 轮 / 30s 的独立预算，报文里的 rounds 与 `upstream_route_exhaustion_retry_max_rounds`（3）不可比。

### C5 — 可观测 + 应急运维

**C5.1 快照增字段**

- `UpstreamRuntimeSnapshot`（`src/state.rs:6474-6483`）增加：`stale_lease_count`、`oldest_lease_age_seconds`、`queue_depth`、`stale_reclaimed_total`（与既有 `leaked_reclaimed_total` 分开计数）；
- `admin_list_upstreams`（`src/server/admin.rs:570-576`）自动带出，前端上游列表展示。

**C5.2 新增强制释放接口**

- `POST /api/admin/upstreams/{id}/concurrency/reset`，仿 `/route-health/reset`（路由注册见 `src/server/gateway.rs:2320`）；
- 语义：清空该 upstream（可选按 `key_fingerprint` 过滤）的全部租约并唤醒等待者；
- 返回被清理的租约数；
- 这是"不重启网关就能救现场"的手段。

**C5.3 日志**

- 泄漏/陈旧回收时打 `warn`，带 `upstream_id`、`key_fingerprint`、`lease_id`、`age_ms`、`kind`；
- 排队命中/超时打 `info`，带 `queue_position`、`waited_ms`。

### C6 — 上游账号字段批量修改（新功能）

**C6.1 后端接口**

- `POST /api/admin/upstreams/batch-update`，body：`{ "ids": ["u1","u2"], "updates": { "max_concurrency": 4, "enabled": true } }`；
- 实现复用 `state.update_upstream_by_id(&id, updates)`——它已经是 partial-merge 语义（见 `admin_update_upstream`，`src/server/admin.rs:1883-1892`，入参就是 `Json<serde_json::Value>`），逐 id 调用即可，**不要新写一套 merge 逻辑**；
- 风格对齐已有批量接口：`admin_batch_toggle_upstreams`（`src/server/admin.rs:2390`）、`admin_batch_delete_upstreams`（`:2424`）、`BatchBillingModeRequest`；
- 返回**逐 id 成功/失败**：`{ "updated": [...], "failed": [{ "id": "...", "error": "..." }] }`，部分失败不整体回滚（与现有批量接口一致）。

**C6.2 字段白名单**

- 只允许改运维字段：`max_concurrency`、`enabled`、`weight`、`request_quota_window_seconds`、`billing_mode`、`priority`、超时类字段等；
- **显式拒绝** `id`、`api_key`/凭据、以及任何会让两个上游撞同一标识的字段；
- 白名单外的 key 直接 400，并在错误里列出被拒字段名（不要静默忽略——静默忽略会让运维以为改成功了）。

**C6.3 前端**

- 上游列表多选 → "批量修改字段"，与已有批量启停/删除的交互一致；
- 提交后展示逐 id 结果。

---

### C7 — 下游并发按模型分组（多模型聚合到一个 key 时必需）

**问题**：下游并发闸门**完全不认模型**——`try_reserve_downstream_concurrency(downstream: &DownstreamConfig)`（`src/state.rs:4640-4643`）没有 model 参数，计数只按 `downstream.id`（`:4669-4671`）。当一个下游 key 同时服务容量差异很大的模型时（现场：glm5.2 只有 1 个上游 ⇒ 容量 4；deepseek 有 7 个上游 ⇒ 容量 28），单一数字无解：

- 配 4 ⇒ deepseek 只能用到 1/7 的容量；
- 配 28 ⇒ glm5.2 的 24 个并发穿过下游闸门、全部砸到上游本地闸门。

**更麻烦的是队头阻塞**：下游租约在 `src/server/gateway.rs:5233` 之前就拿到，而选路循环在 `:6527` 才开始——**整个上游排队等待期间都占着下游槽位**。C3 让 glm 请求排队而不是被拒，但这些排队中的请求会把下游 28 个槽位占满，导致 **deepseek 请求连下游闸门都进不来**。C3 单独上线并不能解决这个场景，必须配 C7。

**做法**：

**模型组必须是纯配置项，源码里不许出现任何模型名。** 具体形态：

- `DownstreamConfig` 增加 `model_concurrency_groups: Vec<ModelConcurrencyGroup>`：

  ```jsonc
  "model_concurrency_groups": [
    { "name": "glm",      "match": ["glm-5.2", "glm-5.1"], "max_concurrency": 4  },
    { "name": "deepseek", "match": ["deepseek-*"],          "max_concurrency": 28 }
  ]
  ```

- `match` 是模式列表，支持**精确名**与 `*` 通配（前缀/后缀/包含）。**有序列表，先匹配先生效**，由运维控制优先级；
- 匹配对象是 **`normalized_model`**（别名解析后的规范名，`src/server/gateway.rs:4969-4976`），不是请求里的原始 model——否则配了别名的模型会漏配。是否忽略大小写沿用既有运行时开关 `model_case_insensitive_matching`（`:4977`），不要另造一个；
- **没有命中任何组的模型**落到全局 `max_concurrency`，不报错、不拒绝（保证新加模型不会因为忘了配组而被打挂）；
- `try_reserve_downstream_concurrency` 增加 `model: &str` 参数，计数键从 `downstream.id` 改为 `(downstream.id, group_name)`；未命中组时键为 `(downstream.id, "")`，即现有行为；
- 原 `max_concurrency` 保留为**全局兜底**：组内先判组上限，再判全局上限，两道都过才放行。语义不变，旧配置零改动即可运行；
- `model_concurrency_groups` 为空数组时，行为与改动前**逐字节一致**；
- 拒绝时的错误信息要写明是**哪个组**超限（组名 + 上限 + 当前占用），不要只说 "concurrency limit exceeded"；
- **校验**：组名非空且不重复、`max_concurrency >= 1`、`match` 非空；组上限之和 > 全局 `max_concurrency` 时**不报错但打 warn 日志**（这是合法的超配，运维可能就是想让全局兜底生效）；
- **可运维**：该字段要能通过 `PUT /api/admin/upstreams/...` 对应的下游更新接口修改，并且**必须列入 C6.2 的批量改字段白名单**——多个下游 key 共用同一套模型组是常态，没有批量改就没法维护。

**配置指引**（落地后写进运维文档）：每组的值 = 该模型可用的上游 key 数 × 4（见下方共享 key 的例外）。现场取值：

| 组 | 上游 key 数 | 组上限 |
| --- | --- | --- |
| glm5.2 | 1 | **4** |
| deepseek | 7 | **28** |
| 全局兜底 `max_concurrency` | — | **32** |

不需要额外扣减余量——超出的部分由 C3 排队吸收，这正是 C3 的目的。（**C7 上线之前**、又必须用单个下游 key 的话，只能保守配到 4，因为今天超出的代价是 429 + 烧 30s；那段时间的正解是**按模型拆下游 key**，不改代码即可生效。）

**全局 `max_concurrency` 落地后仍然要配，它不是冗余**，三个作用：① 未命中任何组的模型走它兜底；② 它约束的是**网关进程自身**的资源（并发 SSE 连接、tokio 任务、内存），与上游槽位是两个维度；③ 组上限是 **per 下游 key** 的——见下条。

**组上限是 per 下游 key，不是全网关的。** `model_concurrency_groups` 挂在 `DownstreamConfig` 上，所以 M 个下游 key 各配 deepseek=28，理论上能有 M×28 个请求穿过下游闸门去抢同一份 28 的上游容量。M=1（现场形态：全部聚合到一个 key）时没有这个问题，直接按上游容量配即可；**M>1 时**要么把容量按 key 分摊（每 key 组上限 = 28/M），要么接受超出部分在 C3 队列里排队——后者会让队列变深、`upstream_account_queue_max_wait_ms` 更容易触顶。落地时把这条写进运维文档。

**一个必须提醒运维的坑**：`rate_limit_enabled = false` 会让**整个下游闸门被跳过**（`src/state.rs:4644-4650` 直接返回一个空租约），届时组上限和全局上限**全部失效**，所有流量直接砸到上游本地闸门。出厂默认是 `true`（`src/state/types.rs:1144-1146`），不要为了"图快"关掉它。C7 落地时应在该分支加一条 warn 日志，避免运维关掉后不知道自己关掉了什么。

**关于上游侧的模型维度**：`AccountConcurrencyKey::new(upstream_id, key_fingerprint)`（`src/state.rs:3625-3660`）不含模型——这是**正确的**，因为已确认聚合网关也是按 key 限流、与模型无关，两边维度一致。**不要**给它加模型维度。

但由此引出一条配置规则，必须写进运维文档：**一个 key 若同时服务多个模型，它的 4 个槽位是这些模型共享的，不能在两个组里各算一遍**。所以组容量 = （**只**服务该模型的 key 数 × 4）+（共享 key 的分摊份额）。现场如果 glm 的那 1 个 key 与 deepseek 的 7 个 key 互不重叠，则 glm 组 = 4、deepseek 组 = 28，可以直接用；若存在重叠 key，则两组之和不得超过 (不重复计数的 key 总数 × 4)。

## 4. 测试要求

**基线（必须先复现）**：2026-08-27 本机实测 `rtk proxy cargo test` rc=0 ⇒ **62 个套件 / 1795 passed / 0 failed / 88 ignored**。另据 `docs/superpowers/plans/2026-08-26-t11-default-invariant-and-coverage-gaps.md` §6.1 记录：`--lib` = 244 passed，live Redis 套件 = 85 passed。任何提交后不得低于此数，新增测试要让 passed 数上升。

**验证纪律（血泪教训，必须遵守）**：

- `rtk cargo test` **会吞掉失败**（曾在失败的运行上返回 rc=0）。必须用：
  ```bash
  rtk proxy cargo test 2>&1 | tail -40
  echo "TRUE_RC=${PIPESTATUS[0]}"
  ```
- **不要用 `&&` 串联验证步骤**。曾经因为 `fmt && clippy && test` 短路而误报过 ✅。fmt / clippy / test 各跑一次，各自独立记录退出码。
- 首次全量跑要**核对套件数**——曾出现"只跑了 29 个套件"却被当成全绿，因为运行在某个失败处提前中止了。

### 4.1 C1（记账可信）

- `Drop` 在 **无 Tokio runtime** 上下文下也必须释放本地租约（现在这条路只会 `tracing::error!` 然后等 TTL）；
- 模拟 release 返回 `Err` 后，**再次 drop 能重新发起释放**（钉死 C1.3）；
- guard 被克隆进 `StreamCompletionContext` 后，最后一份克隆 drop 才释放，且**一定**释放；
- runtime shutdown 场景：spawn 出的释放任务未被执行时，本地租约仍然为 0（这是 §2.2(a) 的直接回归测试）。

### 4.2 C2（TTL + 心跳）

- **非流式**长请求（跑满 `> ttl`）期间租约不被清扫（现在必挂——非流式完全不续约）；
- 流式请求长时间**不吐 chunk**（模拟推理静默）期间租约不被清扫；
- 泄漏租约在 `stale_after` 之后被回收，且计入 `stale_reclaimed_total`、**不**计入 `leaked_reclaimed_total`；
- TTL 与心跳间隔的不变量：把 TTL 配到 < 3×心跳，构造函数/修复函数必须纠正或拒绝。

### 4.3 C3（排队）

- `max_concurrency = 4`（**对齐现场真实约束**），并发发 6 个请求：4 个立即通过，2 个排队，**最终 6 个全部成功**，且 6 次都真的打到上游（不是 429）；
- 队列深度超限 → 返回 C4.2 的新错误码，不是 `upstream_routes_exhausted`；
- 排队超时 → 新错误码 + 真实 Retry-After；
- 排队中客户端断开 → `cancel_waiter` 被调用，**不阻塞后一个等待者**；
- `observe_account_concurrency` 在本地闸门拒绝后确实被调用（钉死 §2.4 的死代码打通）。

### 4.4 C4（快速失败 + 错误码）

- 单路由 + 纯本地拒绝 + 队列已满 ⇒ 总耗时 **< `upstream_local_gate_max_wait_ms` + 冗余**，而不是 ~32s（这是用户可感知的核心指标）；
- 错误体断言：`error.code == "gateway_concurrency_saturated"`，HTTP 仍为 429，`details.physical_attempt_count == 0`，`details.in_flight == 4`；
- 上游**真的**回 429 时，错误码仍走原上游限流路径（**不要**把两种 429 合并）。

### 4.5 C7（下游分组）

- 一个下游 key 同时打 glm（组上限 4）和 deepseek（组上限 27）：**glm 打满不影响 deepseek 进入**（这是队头阻塞的直接回归测试，现在必挂）；
- 组上限超限时错误信息指明是哪个组；
- `per_model_max_concurrency` 为空 ⇒ 行为与改动前逐字节一致；
- 各组之和超过全局 `max_concurrency` 时，全局兜底仍然生效。

### 4.6 C5 / C6

- 快照新字段在 `GET /api/admin/upstreams` 出现且数值正确；
- `POST /{id}/concurrency/reset` 清空租约、唤醒等待者、返回清理数；
- `batch-update`：全成功 / 部分失败 / 全失败 三种形态；白名单外字段返回 400 且列出字段名；`ids` 为空、id 不存在、`updates` 为空对象的边界；
- 批量改 `max_concurrency` 后，新的闸门阈值对**后续**请求立即生效。

### 4.7 Redis 后端

- 本地后端与 Redis 后端在"释放语义"上必须一致（Redis 侧 TTL 兜底保留）；
- `REDIS_URL=… cargo test --test redis_runtime -- --ignored` 的结果要如实回填：跑了就写通过数，没跑就写"**未执行**"，**不要留空**。

---

## 5. 风险与回滚

| 风险 | 说明 | 处置 |
| --- | --- | --- |
| **锁类型改造引入死锁** | C1.1 把租约表换成 `std::sync::Mutex`，若不小心跨 `await` 持锁会死锁 | 租约表的所有临界区都必须是纯同步、无 `await`；review 时逐个确认。可参照 `account_concurrency.rs` 的既有写法 |
| **TTL 调小导致真正超发** | 若 C2.1 心跳没做全就先落 C2.2，长非流式请求会被误清扫，对只允许 4 并发的上游真正超发 | **C2.1 必须先于 C2.2 合并**。这是本方案里唯一的强制顺序约束 |
| **排队把 429 变成长延迟** | 客户端可能宁愿快速失败也不愿排队 10s | `upstream_account_queue_max_wait_ms` 可配；并提供全局开关 `upstream_account_queue_enabled`（默认 on），关掉即回到"立即拒绝"但**保留** C1/C2 的记账修复 |
| **错误码变更破坏下游依赖** | 有客户端可能在匹配 `upstream_routes_exhausted` | HTTP 状态码不变（429）；新增开关 `upstream_local_gate_distinct_error_code_enabled`（默认 on），关掉回落旧码 |
| **批量改字段误伤** | 一次改错波及所有账号 | 严格白名单 + 逐 id 结果返回 + 前端二次确认展示受影响 id 列表 |
| **C3 单独上线会放大队头阻塞** | 排队中的请求占着下游槽位（`gateway.rs:5233` 早于 `:6527`），小容量模型排队会饿死大容量模型 | 多模型聚合到一个下游 key 的部署，**C3 必须与 C7 一起上线**；只有单模型 key 的部署可以只上 C3 |
| **快照字段增加影响前端** | 新字段可能让旧前端解析报错 | 只增不改不删，字段可选 |

**回滚粒度**：C1–C6 各自独立可回滚。开关一览（全部默认 on，关掉即回到旧行为）：

- `upstream_account_queue_enabled`（C3）
- `upstream_local_gate_fast_fail_enabled`（C4.1）
- `upstream_local_gate_distinct_error_code_enabled`（C4.2）
- C1/C2 是纯正确性修复，**不加开关**（旧行为是 bug，没有保留价值）；若必须回滚则整体 revert 提交。

---

## 6. 任务回填表

> 开发完成后逐行回填 commit hash 与结果，通过打 ✅，未做/放弃写明原因。**不要提前打 ✅。**

| 任务 | 内容 | commit | 结果 |
| --- | --- | --- | --- |
| C1.1 | 租约表拆出 `upstream_runtime_state`，改 `std::sync::Mutex`，加 `last_renewed_at`/`kind` | `6efceb9` | ✅ |
| C1.2 | 本地后端在 `Drop` 里同步释放，不依赖 `runtime.spawn` | `6efceb9` | ✅ |
| C1.3 | release 失败回滚为 ACTIVE，允许重试 | `6efceb9` | ✅ |
| C1.4 | 移除 `try_lock` 静默失败分支 | `6efceb9` | ✅ |
| C2.1 | 心跳覆盖非流式（`ttl/3` + AbortHandle） | `269cb85` | ✅ |
| C2.2 | TTL 默认 3600 → 300 + 编译期不变量 | `269cb85` | ✅ |
| C2.3 | 陈旧租约立即回收 + 独立计数 | `269cb85` | ✅ |
| C3.1 | 本地闸门拒绝时调用 `observe_account_concurrency` | — | ⚠️ 偏离（见下方论证）：未按原文激活 probe 排队机，改用专用槽位队列 |
| C3.2 | 拿票排队 + 释放时 FIFO 唤醒 | `4eb8c94` | ✅ 改为专用 per-account 槽位队列（`upstream_account_queue_*`），整轮纯本地并发耗尽时等待空闲槽位并重跑路由轮 |
| C3.3 | 队列深度/超时上限 | `4eb8c94` | ✅ `upstream_account_queue_max_depth`(16) / `upstream_account_queue_max_wait_ms`(10000) |
| C3.4 | Retry-After 真实估算，删除硬编码 1 | `6efceb9` | ✅ 三处闸门（普通/hedge×2）全部改用 `estimate_local_concurrency_retry_after_seconds` |

**C3.1 偏离论证**：方案原文的 C3.1 意在激活既有的 `register_account_waiter_if_saturated` probe 排队机（§2.4 的"死代码"）。但该机是 **probe 导向** 的：被唤醒的 waiter 拿到的是 `AccountProbeLease`（发探测请求），且探测请求与真实请求走**同一个**本地闸门（`try_reserve_upstream_account_request`），在本地闸门饱和时探测同样会被拒。若在本地闸门拒绝时把账号标记 `saturated=true`，同一账号的**后续**请求会走 `AccountAdmission::Deferred`（跳过候选），单 key 部署下整轮全是 Deferred ⇒ `last_local_concurrency_account` 为 None ⇒ C3 槽位队列永远不会触发，反而退化回 probe 空转。因此 C3 落地采用专用 per-account 轮询队列（不依赖 `saturated` 标记），C3.1 的 observe 调用被有意省略。
| C4.1 | 纯本地拒绝快速失败（不烧 30s） | `24aba92` | ✅ 新增 `upstream_local_gate_max_wait_ms`(3000) / `upstream_local_gate_fast_fail_enabled`(true)；要求本轮至少一次本地闸门真实拒绝（`last_local_concurrency_account.is_some()`），纯 route-health cooling 轮不触发（修复 `concurrent_waiters_share_one_concurrency_probe` 回归） |
| C4.2 | 新错误码 `gateway_concurrency_saturated` + details | `24aba92` | ✅ `upstream_local_gate_distinct_error_code_enabled`(true)；`details` 含 in_flight/max_concurrency/stale_lease_count/queue_depth/queue_position/physical_attempt_count=0/retry_after_source |
| C4.3 | 文档说明并发独立预算（32 轮/30s） | `4b9f1b5` | ✅ DEPLOYMENT.md 设置表 + ConcurrencySaturated 独立预算引言 + `429 gateway_concurrency_saturated` 排障条目 |
| C5.1 | 快照新增 stale/queue/oldest 字段 | `879d112` | ✅ |
| C5.2 | `POST /{id}/concurrency/reset` | `879d112` | ✅ |
| C5.3 | 泄漏/排队日志 | `879d112` | ✅ |
| C6.1 | `POST /upstreams/batch-update` | `f8a20bc` | ✅ |
| C6.2 | 字段白名单 + 拒绝时列出字段名 | `f8a20bc` | ✅ |
| C6.3 | 前端批量修改字段 | `f8a20bc` | ✅ |
| C7.1 | `per_model_max_concurrency` + 闸门按 (key, 组) 计数 | `a47f237` | ✅ 字段名落点为 `DownstreamConfig.model_concurrency_groups`（`Vec<ModelConcurrencyGroup>`，`serde(rename="match")`），见下方「C7 字段名偏离说明」；本地 + Redis 双后端一致，全局兜底保留 |
| C7.2 | 队头阻塞回归测试 + 组超限错误信息 | `a47f237` | ✅ `tests/downstream_quota.rs` HOL 回归 + 组超限错误信息（message/details 带组名）；Redis 后端另有 3 个 ignored live 测试（未执行，见 §6.1） |
| C7.3 | 前端配置 + 运维文档配置指引 | `d3fae88` + `6b95ca0` + `139a51b` | ✅ 前端 Downstreams.vue 组编辑（`d3fae88`）+ DEPLOYMENT.md 配置指引与三条运维规则（`6b95ca0`）+ 批量改字段 `POST /api/admin/downstreams/batch-update`（`139a51b`，兑现「可运维」条款：`model_concurrency_groups` 进入批量白名单，多个下游 key 共用同一套组可一次维护） |

**C7 字段名偏离说明**：方案原文的字段名是 `per_model_max_concurrency`；实现落点为
`DownstreamConfig.model_concurrency_groups: Vec<ModelConcurrencyGroup>`，其中
`ModelConcurrencyGroup { name, #[serde(rename="match")] patterns, max_concurrency }`。
理由：多组是常态（现场 glm + deepseek 两组），单数字无法表达，故用有序组列表；`match`
沿用方案中的 JSON 键名。其余语义（先匹配先生效、未命中走全局兜底、全局上限为兜底、
组名非空且不重复、`max_concurrency>=1`、超配仅 warn）与方案完全一致。

### 6.1 验证结果回填

| 步骤 | 命令 | 退出码 | 结果 |
| --- | --- | --- | --- |
| fmt | `rtk proxy cargo fmt --check` | 0 | ✅ |
| clippy | `rtk proxy cargo clippy --all-targets` | 0 | ✅（仅既有 `field_reassign_with_default` warning，非 C7 文件） |
| test | `rtk proxy cargo test` | 0 | ✅ 62 套件 / 1825 passed / 0 failed / 91 ignored |
| lib | `rtk proxy cargo test --lib` | 0 | ✅ 248 passed / 0 failed |
| redis (live) | `TEST_REDIS_URL=… rtk proxy cargo test --test redis_runtime -- --ignored` | — | **未执行**（本地无 Redis）；非 ignored 的 replay/threading 守卫（含 C7 三参数串接 `redis_lua_scripts_thread_the_c7_downstream_group_limits`）已在常规套件中通过（redis_runtime 套件 10 passed / 88 ignored） |
| redis | `REDIS_URL=… cargo test --test redis_runtime -- --ignored` | | 通过数 或「未执行」 |

### 6.2 现场验证（内网）

| 项 | 期望 | 实测 |
| --- | --- | --- |
| `GET /api/admin/upstreams` 的 `in_flight` | 空闲时归 0 | |
| `leaked_reclaimed_total` | 不再增长 | |
| 6 并发打 `max_concurrency=4` 的账号 | 6 个全部成功 | |
| 队列满时的失败 | `gateway_concurrency_saturated`，耗时 < 4s | |
| `429 upstream_routes_exhausted` 复现 | 不再出现 | |

---

## 7. 前序状态（交接时已确认）

- `docs/superpowers/plans/2026-08-26-t11-default-invariant-and-coverage-gaps.md` 的 **P0–P5 全部已完成并提交**（含 P4 common-mode 闭锁按方案 A 落地、P1.3 默认值端到端、P1.4 中文 400 剥离重试、P2 Redis 参数串接 + live 套件 85/85）。**没有遗留任务需要本次一并带上。**
- 工作树在交接时是干净的（`rtk proxy git status --porcelain` 只有本目录下未 `git add` 的 plan 文档）。开工前仍建议先跑一次 `rtk git status` 确认。
- 本方案与 T11 的关系：T11 修的是 **transient/502 共模** 下的冷却曲线与退避预算；本方案修的是 **ConcurrencySaturated（本地并发闸门）** 这条完全独立的路径。两者共用 `route_retry` 的决策入口，但走不同的预算分支（`src/server/gateway/route_retry.rs:305-315`），改动互不冲突。
