# 方案：路由耗尽的必然性根因（冷却上界 ≥ 等待预算）+ 国内模型 400 自愈缺口

日期：2026-08-25
状态：待开发（交由其他模型实施）
关联：`2026-08-25-upstream-400-diagnosis.md`（P1 logprobs 白名单，已合入 `f7dd7ad6`）、
`2026-08-13-route-exhaustion-self-healing-and-model-alias-unification.md`（Part A A1-A5）、
`2026-08-21-continuation-pin-escape.md`（P2-P6）、`2026-08-22-upstream-error-code-surfacing.md`（E1-E6）

---

## 0. 结论先行（根因已闭环，无推断缺口）

**上一轮 P1（把 `logprobs`/`top_logprobs` 加进方言剥离白名单）方向对，但只处理了 400 的一个分支，完全没有触及 `upstream_routes_exhausted` 的产生机制。**

### 根因一句话

> **聚合网关在 502 响应里带了 `Retry-After: 28`。本网关把这个 header 当成"路由摘除时长"无条件写进健康注册表（只被 `upstream_retry_after_cap_seconds`=30s 截断），而请求内轮间等待预算 `retry_max_wait` 恰好也是 30s。28s 冷却 > 30s−已耗预算 ⇒ `RouteRetryPolicy` 必然返回 `GiveUpReason::WaitBudget` ⇒ 一轮轮间等待都不做就耗尽。**

`max_rounds=3`、budget alignment、last-resort 半开探测这三个自愈机制**一个都没有机会执行**。
这不是概率问题，是给定 `Retry-After ∈ [28, 30]` 时**数学上确定**的结果。

### 证据链（每一跳都有 file:line，无一处靠推断）

| # | 事实 | 证据 |
|---|------|------|
| 1 | `Retry-After` header 被**无条件**解析，与失败分类无关 | `upstream_feedback.rs:718` `let retry_after = parse_retry_after(input.headers);` 在 `classify_upstream_response` 顶部，早于任何 class 判断 |
| 2 | 解析结果对**所有** class 一律写进结果结构（502/TransientServer 也带） | `upstream_feedback.rs:750-757` `ClassifiedUpstreamFailure { class, …, retry_after, … }`，无 class 过滤 |
| 3 | 纯数字 `28` 直接被解析为 28s | `upstream_feedback.rs:367-369` `value.parse::<u64>() → Duration::from_secs(seconds)` |
| 4 | 该值成为 `GatewayError::Classified.retry_after` | `errors.rs:874-880` `GatewayError::Classified { retry_after, .. } => *retry_after` |
| 5 | 只被 `upstream_retry_after_cap_seconds` 截断；**28 < 30 ⇒ 原样通过** | `gateway.rs:559` `clamp_upstream_retry_after(error.retry_after(), retry_after_cap)`；`gateway.rs:653-655` 实现是 `.min(cap)` |
| 6 | 写进 attempt ledger，成为日志里的 `cooldown_seconds` | `gateway.rs:560-567` → `gateway.rs:8054-8062` |
| 7 | **同一个值**成为路由冷却时长，且只能"抬高"不能"压低" | `route_health.rs:1353-1358` `(_, Some(explicit)) => explicit.max(local)` |
| 8 | 该值成为 `TerminalFailure::Temporary{retry_after}`（多候选取 `.min()`） | `route_attempts.rs:847-853` |
| 9 | 重试策略拿它当 `required_delay`，与剩余预算比较后放弃 | `route_retry.rs:349-358` `if sleep_for > remaining { return (None, Some(GiveUpReason::WaitBudget)) }` |
| 10 | 剩余预算已被同路由重试提前吃掉一部分 | `gateway.rs:7187` `route_retry_budget.record_wait_time(retry_delay)`；`gateway.rs:7159-7161` 每次 clamp 到 200ms~2s |

### 关键排除性证据（为什么"本地退避升级到 step 4"被排除）

这一条决定了修哪里，必须讲清楚：

- `record_failure_with_status` 在整个 gateway.rs 里**只有一个调用点**（`gateway.rs:560`），
  另一处 `record_failure`（`gateway.rs:3349`）传的是 `None`。
- 也就是说 **attempt ledger 里的 `retry_after` 只可能是"截断后的上游 header"**，
  本地 `jittered_backoff`（`route_health.rs:2013-2025`）算出的冷却值
  **从来没有被写回 ledger**——它只活在健康注册表里。
- 而 `cooldown_seconds` 恰恰取自 ledger（`gateway.rs:8054` 的 `terminal_observation.retry_after`）。
- 反证：若上游**没有**发 `Retry-After`，则所有 temporary 候选的 `retry_after` 都是 `None`，
  `route_attempts.rs:852` 的 `.unwrap_or(Duration::from_secs(1))` 会让日志显示
  **`cooldown_seconds=1`**，绝不可能显示 28。

> **结论：`cooldown_seconds=28` 只能是上游发来的 `Retry-After: 28`。**
> 本地 step 升级（base 3s << 3 = 24s × 抖动 ≈ 19~29s）虽然数值上也能凑出 28，
> 但它没有通往这条日志字段的代码路径，**予以排除**。
>
> 这同时解释了一个此前无解的现象：
> **按 `DEPLOYMENT.md:203` 把 `upstream_transient_route_cooldown_base_seconds` 调到 2~3s，冷却时长毫无变化。**
> 因为 `explicit.max(local)` 里 `explicit`=28 永远赢，运维那个旋钮在有上游 header 时是空转的。

### 三个默认值的致命撞车（都在 `src/state/types.rs`）

| 常量 | 默认 | 行 |
|------|------|----|
| `DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_WAIT_MS` | **30_000** | `types.rs:108` |
| `DEFAULT_UPSTREAM_RETRY_AFTER_CAP_SECONDS` | **30** | `types.rs:120` |
| `DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_SECONDS` | **300** | `types.rs:95` |

`retry_after_cap`(30s) **≥** `retry_max_wait`(30s)。
**只要上游发出的 `Retry-After` 落在 [约 25, 30] 区间，重试循环就 100% 退化成 0 轮等待。**
没有任何代码校验这两个字段的关系（`runtime_settings.rs:511` 只做 `1..=3600` 的独立范围检查）。

### 修复优先级由此确定

1. **T1.2（上游 `Retry-After` 与本地冷却解耦）= 唯一的必要且充分修复**，其余都是加固。
2. **T1.1（不变量校验）** 防止同类撞车换个参数再来一次。
3. **T2.1（探测可达）** 让已故障但未恢复的池子还有一次自救机会。
4. T1.3 / T1.4 / T2.2 / T2.3 是对"上游不发 header"那条分支（本地 step 升级）的加固——
   那条分支**确实存在且会独立造成耗尽**，只是不是本次日志的成因。
5. T3.* 修 400 直抛与"中文参数错误被误判成瞬态从而冷却路由"的耦合。

---

## 0.5 立即可用的配置级缓解（零代码，今天就能上）

根因锁定在"上游 header → 冷却"这条链上，而这条链上**唯一的截断点是运维可调的**，
所以在任何代码合入之前就能止血。三个设置全部已存在、已在 `1..=3600` 校验范围内：

| 设置 | 现值 | 立即改为 | 作用 |
|------|------|---------|------|
| `upstream_retry_after_cap_seconds` | 30 | **5** | 上游 `Retry-After` 最多只能把冷却抬到 5s（切断本次根因） |
| `upstream_transient_route_cooldown_max_seconds` | 300 | **15** | 本地 step 升级的**硬上限**（切断第二条路径） |
| `upstream_transient_route_cooldown_base_seconds` | 10 | **2** | 放慢升级速度，前 3 级都在 8s 内 |

**为什么这组值可证明有效**（不是调参试运气）：

- 有效冷却 = `explicit.max(local)`（`route_health.rs:1355`）
  = `max(≤5s, local)`
- `local` 由 `jittered_backoff` 产出，而该函数**先抖动、后 `.min(max)`**
  （`route_health.rs:2019-2023`：`.saturating_mul(jitter_percent).saturating_div(100).min(max.as_nanos())`）
  ⇒ **`local ≤ 15s` 是硬保证**，与 step 涨到多少无关
- ⇒ 有效冷却 **≤ 15s**（硬上界）
- 轮间等待预算 `remaining = 30s − waited`，`waited` 最坏约 6s ⇒ `remaining ≥ 24s`
- ⇒ `sleep_for ≈ 15s + 0.1s < 24s` ⇒ `route_retry.rs:355` 的 `sleep_for > remaining` **不再命中**
- ⇒ **轮间等待恢复，`max_rounds=3` 与 last-resort 探测重新可达**

**副作用（可接受，需知情）：**

1. 回给下游客户端的 `Retry-After` header 同样被压到 ≤5s（`errors.rs:1159`）——
   对 codex 这类会自动重试的客户端反而更好。
2. `ConcurrencySaturated` 类的冷却直接取 `explicit`（`route_health.rs:1354`，**不取 max**），
   也会被压到 ≤5s。若上游存在真实并发槽位限制，会出现更频繁的探测。
   **这正是 T1.2 要拆成独立旋钮的原因**——缓解期先接受，代码修完后回调 cap 到 30。
3. 真实长时间故障（上游整体宕机）会被更频繁重试，上游压力上升。
   共模断路器与 step 升级仍在，只是天花板降到 15s。

**验证缓解是否生效（改完 10 分钟内即可判断）：**

```bash
# cooldown_seconds 应从 28 降到 ≤15，且不再出现 routing_round=1 的耗尽
rtk grep -a 'routes_exhausted' logs/*.log | rtk proxy grep -ao 'cooldown_seconds=[0-9]*' | sort -t= -k2 -n | tail -5
rtk grep -a 'routes_exhausted' logs/*.log | rtk proxy grep -ao 'routing_round=[0-9]*' | sort | uniq -c
```

> **缓解 ≠ 修复。** 这组值把安全边界从"负数"抬到了 9s，但**依然没有任何机制保证它不被改回去**
> ——这就是 T1.1 必须落地的原因。T1.2 之后 cap 应回调至 30（只影响下游 header），
> 由新的 `upstream_retry_after_cooldown_cap_seconds` 独立控制健康冷却。

## 1. 报错链路复盘（对着这次的日志逐字段还原）

### 1.1 终态日志的产生点

`src/server/gateway.rs:8068-8091` 的 `tracing::error!("request failed after exhausting upstream candidates")`。

对照用户日志逐字段：

| 日志字段 | 代码来源 | 本次含义 |
|---------|---------|---------|
| `upstream_status=502` | `gateway.rs:8043-8047` | 终态观测的上游状态 |
| `failure_class=transient_server` | `gateway.rs:8048-8052` | **被判成"服务瞬态故障"⇒ 要冷却路由**（若判成 `RequestRejected` 则 `gateway.rs:541-543` 直接 return，根本不记失败） |
| `cooldown_seconds=28` | `gateway.rs:8054-8062` | **= 上游 `Retry-After: 28`**（见 §0 证据链与排除性证据） |
| `routing_round=1` | `request_route_attempts.routing_round()` | **只有 1 轮，没有任何轮间等待发生** |
| `physical_attempt_count=6` | `gateway.rs:8084` | 第 1 轮内真实打出去 6 次请求 |
| `same_route_retry=true` | `any_same_route_retry`, `gateway.rs:7156` | 同路由重试触发过（每条路由 1 次），**并因此吃掉了等待预算** |
| `remaining_candidates=0` | **硬编码字面量** `gateway.rs:8081` | **不携带任何信息，见 R11** |
| `half_open_busy_count=0` | `attempt_ledger.half_open_busy_count()` | 不是 T3 busy 路径 |
| `account_recovery_rounds=0` | `account_recovery.rounds()` | 不是并发账号恢复路径 |
| `continuation_candidate_count=2` | `gateway.rs:5713` | **不是"2 条候选路由"，是 2 个 (能力档位 × 协议) 通道，见 R12** |
| `continuation_pin_escaped=false` | `gateway.rs:8090` | 续写 pin 逃逸未触发 |

### 1.2 为什么 `routing_round` 停在 1（决定性推导）

`src/server/gateway/route_retry.rs:349-358`，普通（非 round-cap）路径：

```rust
let required_delay = health_recovery.map(|r| r.retry_after).unwrap_or(retry_after);   // = 28s
let next_round = budget.current_round.saturating_add(1);
let jitter = deterministic_jitter(request_id, next_round);                            // 0..100ms
let Some(sleep_for) = required_delay.checked_add(jitter) else { ... };                // ≈ 28.0~28.1s
let remaining = max_wait.saturating_sub(budget.waited);                               // 30s − waited
if sleep_for > remaining {
    return (None, Some(GiveUpReason::WaitBudget));                                    // ← 命中
}
```

`budget.waited` 在本轮已被同路由重试吃掉：`gateway.rs:7187` 的
`route_retry_budget.record_wait_time(retry_delay)`，每次 200ms~2s（`gateway.rs:7159-7161`）。
6 次物理尝试里有 3 次是同路由重试 ⇒ `waited ≈ 0.6~6s` ⇒ `remaining ≈ 24~29.4s < 28.1s`
⇒ **`WaitBudget` 放弃。**

注意：**即使 `waited = 0`**，`remaining = 30s` 而 `sleep_for ≈ 28.05s`，只剩不到 2s 余量；
上游只要发 `Retry-After: 30`（同样 ≤ cap）就必然超出。**这个配置本身没有安全边界。**

`max_rounds=3` 那条分支（`route_retry.rs:290`）根本没走到——`current_round=1 < 3`。
所以 A2 的 budget alignment 也不可能触发（它在 round-cap 分支内，`route_retry.rs:291-330`），
且即便触发，其判据 `sleep_for <= remaining`（`route_retry.rs:307`）与上面同款，同样过不了。

**这就是"改了 logprobs 白名单还是照旧"的直接原因：本次耗尽根本不由参数错误触发，而由 `Retry-After` 与等待预算的量纲撞车触发。**

### 1.3 为什么上游会在 502 上发 `Retry-After`

内网形态是"多条路由 → 同一个 new-api/one-api 聚合网关"。该类网关在上游渠道全忙/熔断时
惯常做法是回 502/503 并附 `Retry-After`（表达"渠道 N 秒后再试"）。
本网关把它当成"**摘除这条路由** N 秒"，但语义其实是"**客户端** N 秒后再来"——
**量纲不同**：前者应由本地退避曲线决定，后者只该影响回给下游的 header。

更糟的是共模：同一 host 的 6 条路由拿到**同一个** `Retry-After: 28`，
于是 6 条路由被同时冷却 28s，下一个请求进来看到的是空池
（此时 `is_edge_proxy_error` 因 body 是 JSON 而非 nginx HTML 返回 false，
`upstream_feedback.rs:554-573`，所以走不到 `EDGE_PROXY_ROUTE_MAX=15s` 那条更温和的曲线）。

### 1.4 本地 step 升级：独立存在的第二条耗尽路径（本次未命中，但必须修）

当上游**不发** `Retry-After` 时，冷却完全由 `jittered_backoff`（`route_health.rs:2013-2025`）
`base << (step-1)` × 80~120% 决定，而：

- 普通（非半开）失败**没有任何 step 上限**（`route_health.rs:1852-1864` 的 `else` 分支是裸 `saturating_add(1)`）
- A1 的抑制只覆盖**请求内**重复（`route_health.rs:1845-1851` 的 `repeat_within_request`），
  codex 客户端按 `please try again in Ns` 发起的外层重试是**新的下游请求** ⇒ 照常 +1
- streak 窗口 600s 内不衰减（`route_health.rs:40`）

⇒ base=10s 默认下 step 3 就是 32~48s，**同样越过 30s 预算**，同样必然 `WaitBudget`。
即使加上半开上限 `ROUTE_HALF_OPEN_FAILURE_STEP_CAP=5`（`route_health.rs:1826`），
base=3 ⇒ `3<<4=48s` ×0.8 = 38.4s，**仍然超预算**——**上限本身从未对齐预算**。

**所以 T1.2 修完本次故障后，T1.1 + T1.3 必须跟上，否则换个上游（不发 header 的）同样耗尽。**

---

## 2. 问题清单（12 条根因，按修复优先级）

### A 组：耗尽的必然性（必须修，否则一切自愈都是装饰）

#### R1 — 冷却上界 ≥ 请求内等待预算，重试循环退化为 0 轮等待 【放大器·决定性】

- 证据：`types.rs:108`（`max_wait=30s`）、`types.rs:120`（`retry_after_cap=30s`）、
  `types.rs:95`（`cooldown_max=300s`）、`route_retry.rs:349-358`（`sleep_for > remaining → WaitBudget`）
- 后果：`routing_round` 恒为 1；`max_rounds`、budget alignment 全部失效。
- **不存在任何校验保证这个不变量**：`src/state/runtime_settings.rs:511` 只对
  `upstream_retry_after_cap_seconds` 做 `1..=3600` 的独立范围检查，字段之间的关系无人校验。
- 即使走半开失败上限 `ROUTE_HALF_OPEN_FAILURE_STEP_CAP = 5`（`route_health.rs:1826`），
  base=3 ⇒ `3 << 4 = 48s`，×0.8 = 38.4s，**仍然超过 30s 预算**。上限本身没有对齐预算。

#### R2 — 上游 `Retry-After` 无条件作为本地冷却下界 【本次故障的首要根因】

- 证据：`route_health.rs:1355` `(_, Some(explicit)) => explicit.max(local)`
- 后果：内网调低 `cooldown_base` 的运维手段被上游 header 完全覆盖；
  `retry_after_cap` 默认 30s 恰好 ≥ 等待预算 30s，构成 R1 的最坏输入。
- 语义混淆（量纲错误）：上游的 `Retry-After` 说的是"**客户端**多久后再试"，
  不等于"**网关**应该把这条路由摘除多久"。前者应影响回给客户端的 header，后者应由本地退避策略决定。
- **已由 §0 证据链确认这就是本次 `cooldown_seconds=28` 的来源**，非推测。
- 单向性：`explicit.max(local)` 意味着上游 header **只能抬高、永不压低**冷却，
  运维调低 `cooldown_base` 在有 header 时完全空转。
- 共模放大：同 host 的 N 条路由拿到同一个 header 值 ⇒ N 条路由同时冷却同一时长 ⇒ 池子整体消失。

#### R3 — 非半开的普通失败没有任何 step 上限

- 证据：`route_health.rs:1852-1864`，只有 `state.half_open_generation.is_some()` 分支
  才 `.min(ROUTE_HALF_OPEN_FAILURE_STEP_CAP)`；`else` 分支是裸的 `saturating_add(1)`。
- 后果：连续 502 的路由 step 可以一路涨到被 `cooldown_max`（默认 300s）截断为止。

#### R4 — 单聚合网关形态下"逐路由冷却"语义错位

- 证据：`gateway.rs:797-804` 已有 `upstream_host()`；
  `gateway.rs:812-820` 的 `CommonModeStreak` 注释明确"same host 失败重启 streak"。
- 后果：共模瞬态断路器（阈值 4）在单 host 下**永不触发** ⇒
  2026-08-12 方案里的"回滚冷却 + 延迟重放"自愈分支在最需要它的部署形态下不可达
  （这是 2026-08-13 方案的 R4，A3 本想用 last-resort 探测补，但被下面的 R5 堵死）。
- 更根本的建模问题：同一 host 上的 N 条"路由"不是 N 个独立故障域。
  把它们逐个冷却，等价于把唯一的物理上游冷却掉，并保证下一个请求进来看到的是空池。

#### R5 — A3 last-resort 半开探测在"本轮打过请求"时结构性不可达

- 证据：`gateway.rs:7833-7838` 的 arm 条件里有 **`round_ledger.attempt_count() == 0`**
- 后果：**第一个遇到故障的请求永远拿不到探测** —— 它进来时路由还没冷却，打出去 502，
  顺手把路由冷却掉，`attempt_count()=6 ≠ 0` ⇒ 不 arm。
  只有后续"进来就全冷"的请求才可能 arm，而那些请求又被 R1 卡在 `WaitBudget` 提前放弃。
  本次日志 `physical_attempt_count=6` 正是这个情形。

#### R6 — 跨请求冷却升级未被抑制（自我放大仍在跑）

- 证据：A1 的抑制只覆盖请求内（`route_health.rs:1845-1851`）；
  P4 只覆盖"续写 pin 收敛到单候选"（`gateway.rs:5713-5720` 的 `sole_contract_candidate`）。
- 后果：codex 每几秒一次外层重试 = 每几秒给每条路由 +1 step，10 分钟窗口内不衰减
  ⇒ 迅速到 step 4~5 ⇒ 冷却 20~40s ⇒ 稳定超过 30s 预算 ⇒ 稳定耗尽。**这是自锁循环。**

### B 组：400 无法自愈（与 A 组耦合，是 A 组的触发源之一）

#### R7 — 方言重试的触发词是纯英文，国内上游中文报错命中不了 【高价值】

- 证据：`src/server/gateway/capability_probe.rs:602-615`

```rust
let indicates_field_error = [
    "unsupported", "not supported", "unrecognized",
    "unknown field", "invalid field", "invalid parameter", "unexpected field",
].iter().any(|pattern| error_lower.contains(pattern));
if !indicates_field_error { return None; }   // ← 中文报错在这里直接返回
```

- GLM / Deepseek / new-api 的常见中文报错：`参数非法`、`参数错误`、`不支持该参数`、
  `无效的参数`、`缺少必需参数`、`请求参数有误` —— **一个都不命中** ⇒
  `dialect_field_error_hint` 返回 `None` ⇒ 不剥字段 ⇒ 400 直抛客户端。
- **覆盖不对称的直接反证**：同一仓库里 `upstream_feedback.rs:492-509` 的
  `message_is_rate_limited` **已经有完整中文词表**（`限流`/`限速`/`频率过高`/`请求过于频繁`/`速率限制`），
  `upstream_feedback.rs:470-475` 的 busy 词表也有 `繁忙`/`过载`/`超载`。
  唯独 `message_is_request_rejected`（`upstream_feedback.rs:512-525`）**只有英文**。
- **这就是 400 与 exhausted 的耦合点**：带 5xx 状态的中文参数错误进到
  `classify_nonsemantic_default`（`upstream_feedback.rs:576-600`），
  因为 `is_explicit_request_rejection` 匹配不上中文 ⇒ **落入 `else` 分支判成 `TransientServer`**
  ⇒ 冷却路由 ⇒ 直接喂给 R6 的跨请求升级 ⇒ 耗尽。
  一个确定性的参数错误，被当成瞬态故障重试到把整池冷却掉。

#### R8 — `correction_for_response` 强制要求 `/error/param`，且 code 白名单只有 OpenAI 三个值

- 证据：`src/server/gateway/dialect_retry.rs:12-25`

```rust
let param = value.pointer("/error/param").and_then(Value::as_str)?;   // ← 无 param 直接 None
...
if !matches!(code, "unsupported_parameter" | "invalid_parameter" | "unknown_field") { return None; }
```

- GLM 返回形如 `{"error":{"code":"1210","message":"..."}}`（数字串 code、**无 `param`**）⇒ `None`。
- Deepseek 返回 `invalid_request_error`（不在白名单）⇒ `None`。
- 于是只剩 generic 路径，而 generic 路径又被 R7 的英文触发词卡住 ⇒ **两条路都堵死**。

#### R9 — `dialect_preset` 是 per-upstream，单聚合网关同挂多模型必然错配

- 证据：`src/state/types.rs:576` `pub dialect_preset: Option<String>`（挂在 `UpstreamConfig` 上）
- `capabilities/types.rs:693-709` 的 `deepseek` 预设：`reasoning_control_field = "reasoning_effort"`，effort 值是**字符串**
- `capabilities/types.rs:710-729` 的 `glm` 预设：`reasoning_control_field = "thinking"`，effort 值是**对象** `{"type":"enabled"}`，并 `omit_sampling_fields += "stream_options"`
- 后果：一个 new-api 上游同时挂 GLM5.1 与 deepseek-v4-flash 时只能选一个预设：
  - 选 `deepseek` ⇒ 给 GLM 发 `reasoning_effort: "high"`（`protocol.rs:600`）⇒ 400
  - 选 `glm` ⇒ 给 Deepseek 发 `thinking: {...}` ⇒ 400
- 学到的档案 `DialectProfileKey` **含** `runtime_model_slug`（`capabilities/types.rs:414-419`），
  所以探测完成后能分开；但**冷启动 / 新增模型 / 探测失效窗口内必然踩**，
  且每次踩都产生一次会冷却路由的 400/5xx。

#### R10 — 剥离白名单与提示词表缺关键字段

- 证据：`capability_probe.rs:618-632` 的字段表 与 `capability_probe.rs:641-655` 的
  `is_safe_dialect_strip_field` 都**没有** `response_format`、`thinking`、`top_p`、`frequency_penalty`、`presence_penalty`
- `protocol.rs:629` 会把 `response_format` 透传进 chat 载荷；GLM 对 `json_schema` 支持不全 ⇒ 400 且不可自愈。

### C 组：可观测性（正是这次排查卡住的原因）

#### R11 — 终态日志缺 `give_up_reason` 【必须先修】

- 证据：`gateway.rs:8068-8091` 输出了 17 个字段，**唯独没有**
  `give_up_reason` / `waited_ms` / `route_count` / `class_counts` /
  `upstream_error_codes` / `live_recovery_seconds` / `last_resort_probe_attempted`。
  这些只进了错误 JSON 的 `details`（`errors.rs:176-230`）。
- 后果：运维从日志**无法区分** `WaitBudget` / `RoundCap` / `NoRecovery` / `AlignmentExhausted` / `HalfOpenBusyCap`。
  本次必须靠读源码反推才能定位到 R1 —— 这是最大的排查成本，也是最容易修的一条。

#### R12 — `remaining_candidates` 是硬编码 0

- 证据：`gateway.rs:8081` 字面量 `remaining_candidates = 0,`
- 后果：日志里的 `remaining_candidates=0` 不携带任何信息，
  但极易被误读成"池子真的空了/没有配置路由"，把排查带偏。

#### R13 — `continuation_candidate_count` 不是路由数（命名与语义不符）

- 证据：`gateway.rs:5713` `let continuation_candidate_count = candidate_passes.len();`
  而 `candidate_passes` 是 `(Option<misses>, protocol)` 组合列表（`gateway.rs:5670-5708`）。
- 后果：`continuation_candidate_count=2` 说的是"2 个 (能力档位 × 协议) 通道"，
  不是"2 条候选路由"。字段名把人骗了。

> 附注：`continuation_pinned` 字段其实**已经**在日志里（`gateway.rs:8088`），
> 用户这次贴的日志没带上。反馈报错时请带完整字段。

---

## 3. 开发任务

> **第 0 步（运维，0 代码）：立即执行 §0.5 的配置级缓解，先止血。**
>
> 排期建议：**T1.2 优先（半天，根治本次故障）** → T1.1 + T1.3（半天，防复发）→
> T0 半天（可观测性，验收依据）→ T1.4 + T2 1~2 天（让自愈在单聚合网关下可达）→
> T3 2~3 天（400 自愈）→ T4 半天（参数与文档）。
>
> 与之前的判断不同：**T0 不再是 T1 的前置**，因为根因已闭环，不需要先采数据。

### T0：可观测性补齐（无行为变更）

> **注意：T0 已不再是定位本次根因的前置条件**——根因已由 §0 证据链闭环。
> T0 的价值变为：(a) 验证 T1/T2 是否真的生效；(b) 下次换个成因时不用再读源码反推。
> 若排期紧张，**T1.2 可以先于 T0 单独上线**。

**T0.1 终态日志补齐放弃原因**

`src/server/gateway.rs:8068-8091` 的 `tracing::error!` 增加字段：

```rust
give_up_reason = request_route_attempts.give_up_reason().map(GiveUpReason::as_str).unwrap_or("none"),
waited_ms = route_retry_budget.waited().as_millis() as u64,
retry_max_wait_ms = runtime_settings.upstream_route_exhaustion_retry_max_wait_ms,
retry_max_rounds = runtime_settings.upstream_route_exhaustion_retry_max_rounds,
route_count = attempt_ledger.distinct_route_count(),
cooled_candidate_count = attempt_ledger.cooled_candidate_count(),
live_recovery_seconds = /* live_recovery 的秒数，None → -1 或省略 */,
last_resort_probe_attempted = request_route_attempts.last_resort_probe_granted(),
upstream_error_codes = %/* ledger.upstream_error_code_counts() 的紧凑串，如 "1210:3,invalid_request_error:2" */,
distinct_upstream_hosts = /* 本请求尝试过的 upstream_host() 去重计数 */,
```

`retry_max_wait_ms` 与 `cooldown_seconds` 并排出现是关键：**运维一眼就能看到 28 > 30−waited 这个矛盾。**

**T0.2 `remaining_candidates` 改成真实值**

删掉 `gateway.rs:8081` 的硬编码 0，改为本请求结束时仍处于可用（非冷却、非半开占用）状态的候选路由数；
若无法在该点低成本取得，则**直接删掉这个字段**，不要留一个恒 0 的误导项。

**T0.3 `continuation_candidate_count` 更名 + 补真实路由数**

- 更名为 `candidate_pass_count`（同步改 `errors.rs:161/228-230` 的 details key，
  在 details 里保留旧 key 一个版本并标注 deprecated）
- 新增 `continuation_route_count`：续写约束生效时**真正通过 contract 过滤的路由条数**
- `gateway.rs:5719` 的 `sole_contract_candidate` 判据也应改用真实路由数
  （当前用 `candidate_passes.len() == 1`，语义同样是错的 —— 这会导致 P4 的
  "单候选不升级 step" 在某些配置下误判或漏判）

**T0.4 冷却写入日志暴露来源与 step**

`src/state/route_health.rs:1345-1360` 附近，在写 `cooldown_until` 时补一条 debug/info 日志（或扩展现有 `step_suppressed` 日志）：

```rust
route_id, class, step, step_suppressed,
local_cooldown_ms,                       // route_cooldown 算出的本地值
upstream_retry_after_ms,                 // clamp 后的上游值（无则 -1）
cooldown_source = "local" | "upstream_retry_after",   // 取 max 后谁胜出
effective_cooldown_ms,
upstream_host,                           // 便于识别共模
```

这条日志是 T1.2 与 T1.3 的**验收依据**：修复后 `cooldown_source` 应基本不再出现
`upstream_retry_after`，且 `effective_cooldown_ms` 应稳定 ≤15000。

**T0.5 采样开启上游错误体摘要**

内网自有上游，`DEPLOYMENT.md:212` 已明确允许：
把 `upstream_error_body_excerpt_enabled` 置 true、`upstream_error_body_excerpt_max_chars` 提到 500，
运行 24h 后统计：

```bash
# 502 的真实 body（判断是 new-api 包装的 400，还是真瞬断）
rtk grep -a 'upstream_error_body_excerpt' logs/*.log | head -50
# 400 的错误码/参数分布
rtk grep -ao 'upstream_error_codes=[^ ]*' logs/*.log | sort | uniq -c | sort -rn
# 冷却来源分布（T0.4 落地后）
rtk grep -ao 'cooldown_source=[a-z_]*' logs/*.log | sort | uniq -c
```

> **这一步要回答的是一个仍未锁定的独立问题**（与本次根因无关，但与"时不时 400"有关）：
> 内网 new-api 是否把上游的 400 包装成 502 下发。
> 若是（body 里能看到 `bad response status code 400` 之类），则需要额外一条任务：
> 在 `classify_nonsemantic_default` 里识别聚合网关包装层，把内嵌 4xx 提升为
> `RequestRejected`（`route_health_outcome` 已经对 `RequestRejected` 返回
> `RouteOutcome::Success`，即**不冷却**，`gateway.rs:686`）。
> `StructuredError::collect`（`upstream_feedback.rs:66-110`）已经会收集嵌套
> `status`/`status_code`/`http_status`/`inner_code`，扩展成本低。

### T1：打破耗尽的必然性（核心）

**T1.1 冷却上界不变量：校验 + 收敛 + 告警**

新增一个显式不变量并在三处强制：

```
effective_cooldown_ceiling = max(
    upstream_retry_after_cooldown_cap_seconds,          // T1.2 新增
    transient_cooldown_base << (transient_cooldown_max_step - 1),   // T1.3 新增
) .min(upstream_transient_route_cooldown_max_seconds)

要求：effective_cooldown_ceiling * 1000 < upstream_route_exhaustion_retry_max_wait_ms
```

1. `src/state/runtime_settings.rs` 的校验函数（`validate` 附近，参考 `:511` 的现有写法）
   增加**跨字段校验**：不满足时拒绝保存并返回可读中文原因，明确写出两个数字。
2. `src/main.rs` 启动时（env 装配后，`:248` 附近）做同样校验：
   不满足则 `tracing::error!` 一条醒目告警 + 自动把 `retry_max_wait_ms`
   抬到 `ceiling * 1.5` 并记录 `auto_corrected=true`（**启动不要 panic**，内网可用性优先）。
3. Admin 前端设置页对这两组字段做联动提示（复用 E6 的设置页改动模式）。

**T1.2 上游 `Retry-After` 与本地冷却解耦** 【本次故障的根治项·最高优先级】

> 这是唯一"必要且充分"修掉本次报错的改动。若只能做一件事，做这件。

- 新增 runtime setting `upstream_retry_after_cooldown_cap_seconds`，默认 **5**，范围 `1..=300`
- `src/server/gateway.rs:663` 的 `route_health_outcome` 里，**用于路由健康的** `retry_after`
  改用这个新 cap 截断；`upstream_retry_after_cap_seconds`（30s）继续只管
  **回给下游客户端的** `Retry-After` header 与终态消息（`errors.rs` 侧不变）
- 语义写进注释：上游的 `Retry-After` 是给**客户端**的建议，不是网关摘除路由的时长依据
- `RouteFailureClass::ConcurrencySaturated` 分支（`route_health.rs:1354`）保持原样
  —— 并发饱和的上游 `Retry-After` 是真实的槽位信息，不能削

**T1.3 非半开失败的 step 上限**

- 新增 runtime setting `upstream_transient_route_cooldown_max_step`，默认 **3**，范围 `1..=8`
- `src/state/route_health.rs:1852-1864` 的 `else` 分支加 `.min(max_step)`
  （与半开分支的 `ROUTE_HALF_OPEN_FAILURE_STEP_CAP` 并存，取更小者）
- 默认 base=10s / max_step=3 ⇒ 上界 40s ×1.2 = 48s，**仍然违反 T1.1 的不变量**
  ⇒ 所以 T4 必须同时把内网默认 base 降到 2s（2 << 2 = 8s，×1.2 = 9.6s < 30s ✓）
- `failure_step` 目前是自由函数（`route_health.rs:1828`），需要把 max_step 透传进去；
  本地与 Redis 两个 backend 行为必须一致（`src/state/redis_runtime.rs`）

**T1.4 shared-host 故障域（内网形态的正解）**

- 新增 runtime setting `upstream_shared_host_failure_domain_enabled`，默认 **true**
- 复用已有的 `upstream_host()`（`gateway.rs:797`）
- 当同一 host 下有 ≥2 条候选路由时，对 `TransientServer` / `EdgeProxyError` 族：
  - 该 host 上所有路由的冷却**取平参照 EdgeProxyError 曲线**
    （`EDGE_PROXY_ROUTE_BASE = 3s` / `EDGE_PROXY_ROUTE_MAX = 15s`，`route_health.rs:32-33`
    —— 这条曲线的形状本来就是为"共享跳瞬断"设计的）
  - **不升级 step**（同 host 的第 2、3 次失败是同一个故障的多次观测，不是独立证据）
- 这一条同时把 R4 的建模问题、R3/R6 的升级问题在内网形态下一并压掉

### T2：让自愈机制真正可达

**T2.1 放宽 last-resort 探测的 arm 条件（修 R5）**

`src/server/gateway.rs:7833-7838`，把

```rust
&& round_ledger.attempt_count() == 0
&& round_ledger.is_all_cooled_transient_family()
```

改为（保持"每请求至多 1 次"不变）：

```rust
&& (
    // 原路径：本轮零物理尝试、全部因冷却被跳过
    (round_ledger.attempt_count() == 0 && round_ledger.is_all_cooled_transient_family())
    // 新路径：本轮打过但全部物理尝试都是瞬态族失败，且此刻池内已无可用候选
    || (round_ledger.attempt_count() > 0
        && round_ledger.is_all_transient_family_failures()   // 需新增
        && round_ledger.available_candidate_count() == 0)    // 需新增
)
```

`AttemptLedger` 需要补两个查询方法（`src/server/gateway/route_attempts.rs`）。
效果：**第一个遇到故障的请求也能拿到一次探测**，而不是必须等到下一个请求进来看到空池。

**T2.2 单 host 的共模瞬态延迟重放（修 R4）**

- 新增 runtime setting `upstream_common_mode_same_host_transient_enabled`，默认 **true**
- `gateway.rs:7632-7640` 附近的 `CommonModeStreak` 判据：
  开关打开时，同 host 的相同 `(class, status)` 失败**也计入** transient streak
  （只放开 transient 族；`RequestRejected` 的请求形状断路器**保持** different-host 语义不变，
  避免误判 —— 那是 2026-08-12 方案刻意做的选择，不要回退）
- 达阈值 ⇒ 走已有的"回滚本请求写入的冷却 + 一次 ≤500ms 延迟重放"分支
  （`common_mode_transient_pool_error`，`gateway.rs:905`），
  这条路径在内网从此可达

**T2.3 budget alignment 从"等不满就不等"改为"能等多少等多少 + 探测"**

`src/server/gateway/route_retry.rs:291-330` 的 alignment 分支与 `:349-358` 的普通分支：
当 `sleep_for > remaining` 且 `remaining >= HALF_OPEN_BUSY_RETRY`（1s）时，
不要直接 `WaitBudget` 放弃，而是**等 `remaining` 然后作为半开探测再打一次**
（与 T2.1 的探测机制复用同一条路），并把这次等待标记为 `alignment_truncated: true`。

理由：现在的"等不满就一秒都不等"是最坏策略 —— 明知 28s 后能恢复、手里还有 24s 预算，
却把 24s 全退给客户端，而客户端只会在几秒后再撞一次并把 step 再推高一格。

### T3：400 自愈与方言覆盖

**T3.1 中文（及多语）报错词表对齐**

- `src/server/gateway/capability_probe.rs:602-615` 的 `indicates_field_error` 补中文：
  `参数非法`、`参数错误`、`参数有误`、`不支持`、`不支持该参数`、`无效的参数`、
  `无效参数`、`缺少必需参数`、`缺少参数`、`非法参数`、`未知字段`、`未知参数`
- `src/upstream_feedback.rs:512-525` 的 `message_is_request_rejected` 补同一组中文
  （与 `:492-509` 的 `message_is_rate_limited` 中文覆盖对齐）
- 注意 `error_lower` 是 `to_ascii_lowercase()`（`capability_probe.rs:603`）：
  中文不受影响，但**不要**改成 unicode lowercase 以免影响既有英文匹配的性能与行为
- **必须同时验证**：修完 R7 之后，中文参数错误在 5xx 上会被
  `classify_nonsemantic_default`（`upstream_feedback.rs:576-600`）判成 `RequestRejected`
  ⇒ `route_health_outcome` 返回 `RouteOutcome::Success`（`gateway.rs:686`）⇒ **不冷却路由**。
  这是 T3.1 对耗尽问题的直接贡献，要有专门的回归测试。

**T3.2 放宽 `correction_for_response` 的入口条件**

`src/server/gateway/dialect_retry.rs:12-25`：

- `/error/param` 缺失时，回退到从 `error.message` 里提取字段名
  （复用 `capability_probe::dialect_field_error_hint`）
- code 白名单增加：`invalid_request_error`、`invalid_parameter_error`、
  `unsupported_value`、`invalid_value`，并接受**纯数字码**
  （GLM 的 `1210`/`1211`/`1214` 族 —— 用"数字码 + message 里有字段名"作为联合判据，
  不要单靠数字码放行）
- 保持"仅 400、仅 `response_started == false`、body ≤ 64KB"三个既有护栏不变

**T3.3 补齐字段表与白名单**

- `capability_probe.rs:618-632` 的字段表补：`response_format`、`thinking`、
  `top_p`、`frequency_penalty`、`presence_penalty`、`seed`、`store`、`metadata`
  （**顺序仍然重要**：更长更具体的先放，避免子串误匹配 —— 已有的
  `top_logprobs` 在 `logprobs` 之前是正确示范，`presence_penalty`/`frequency_penalty`
  与 `penalty` 类似关系同理）
- `is_safe_dialect_strip_field`（`:641-655`）按"移除不改变语义"的标准增补：
  `response_format`、`seed`、`store`、`metadata`、`top_p`、`frequency_penalty`、`presence_penalty` 可加；
  `thinking` **不可**加（它承载推理开关语义，应走 T3.4 的 preset 路径修正而非剥离）
- 每个新增字段都要在注释里写明"为什么移除是安全的"，与现有 `tool_choice`/`reasoning_content`
  被排除的理由（`:637-639`）保持同一标准

**T3.4 per-model dialect preset（修 R9）**

- `src/state/types.rs:576` 旁新增 `UpstreamConfig.model_dialect_presets: BTreeMap<String, String>`
  （模型 slug → preset 名），保留 `dialect_preset` 作为兜底默认
- 解析顺序：**已验证的探测档案（`DialectProfileKey` 含 `runtime_model_slug`）
  > per-model preset > per-upstream preset > baseline**
  （落在 `src/capabilities/resolver.rs:53` 的 `dialect_preset` 入参处；
  改成传入"已按模型解析好的 preset"）
- 持久化：Postgres 迁移一列 JSONB（参考 `src/state/postgres.rs:1862` 的
  `ADD COLUMN IF NOT EXISTS` 写法）+ Admin API（`src/server/admin.rs:1089/1558`）+ 前端
- 内网建议直接配：`{"glm-*": "glm", "deepseek-*": "deepseek"}`（支持前缀通配，
  或复用 `2026-08-13-per-upstream-model-mappings.md` 里已有的匹配约定，避免两套语法）

**T3.5 GLM/Deepseek 保守能力档案预设**

`2026-08-25-upstream-400-diagnosis.md` §8 的 P2.2 项，现在做：
在 `compile_dialect_preset`（`capabilities/types.rs:672`）的 `glm`/`deepseek` 分支里
把已知不支持的字段加进 `omit_sampling_fields`
（`glm` 已有 `stream_options`，按 T0.5 采到的真实 400 分布继续补），
让冷启动的第一发请求就是对的，而不是靠一次 400 去学。

### T4：参数、文档与运维

**T4.1 内网默认参数表**

`DEPLOYMENT.md:185-215` 的 Intranet 小节整体重写为一张**自洽**的参数表，
并明确写出 T1.1 的不变量与推导：

| 设置 | 公网默认 | 内网聚合网关推荐 | 理由 |
|------|---------|----------------|------|
| `upstream_transient_route_cooldown_base_seconds` | 10 | **2** | 共享跳瞬断只有 2~3s |
| `upstream_transient_route_cooldown_max_seconds` | 300 | **15** | 对齐 `EDGE_PROXY_ROUTE_MAX` |
| `upstream_transient_route_cooldown_max_step`（T1.3 新增） | 3 | **3** | 2<<2 = 8s，×1.2 = 9.6s |
| `upstream_retry_after_cooldown_cap_seconds`（T1.2 新增） | 5 | **5** | 上游 header 不得主导本地冷却 |
| `upstream_route_exhaustion_retry_max_wait_ms` | 30_000 | **30_000** | 9.6s ≪ 30s，不变量满足 ✓ |
| `upstream_route_exhaustion_retry_max_rounds` | 3 | **4** | 单 host 池需要更多轮内探测 |
| `upstream_shared_host_failure_domain_enabled`（T1.4 新增） | true | **true** | |
| `upstream_common_mode_same_host_transient_enabled`（T2.2 新增） | true | **true** | |
| `upstream_error_body_excerpt_enabled` | false | **true** | 内网自有上游，诊断必需 |

**T4.2 排查 runbook**

新增一节"`upstream_routes_exhausted` 三步定位法"：

1. 看 `give_up_reason`（T0.1）
   - `wait_budget` ⇒ 不变量被违反 ⇒ 比对 `cooldown_seconds` 与 `retry_max_wait_ms`
   - `round_cap` ⇒ 调 `max_rounds`
   - `no_recovery` ⇒ 健康注册表没有恢复信息，查 Redis/本地 backend
   - `half_open_busy_cap` ⇒ 探测拥塞，调 `half_open_exclusive_window_ms`
2. 看 `cooldown_source`（T0.4）
   - `upstream_retry_after` ⇒ 调 `upstream_retry_after_cooldown_cap_seconds`
   - `local` ⇒ 看 `step`，调 `cooldown_base` / `max_step`
3. 看 `distinct_upstream_hosts`（T0.1）
   - `=1` ⇒ 单聚合网关形态，确认 T1.4 / T2.2 两个开关已开

---

## 4. 测试要求

### 4.1 单元测试

| 模块 | 用例 |
|------|------|
| `route_retry.rs` | 冷却 28s + 预算 30s + 已耗 3s ⇒ `WaitBudget`（**先固化现状 bug**）；T2.3 落地后 ⇒ 截断等待 + `alignment_truncated=true` |
| `route_attempts.rs` | **根因守卫**：ledger 的 `retry_after` 只能来自上游 header——构造一个"上游无 `Retry-After`"的 502 序列，断言 `TerminalFailure::Temporary.retry_after == 1s`（`route_attempts.rs:852` 的 fallback），**证明本地 `jittered_backoff` 值不会漏进 ledger** |
| `upstream_feedback.rs` | **根因守卫**：502 + `Retry-After: 28` header ⇒ `ClassifiedUpstreamFailure.retry_after == Some(28s)` 且 `class == TransientServer`（锁住 `parse_retry_after` 的 class 无关性，防止未来重构悄悄改掉语义）|
| `route_health.rs` | **等价性测试**：`retry_after_cap=5` + `cooldown_max=15` + `base=2` 的配置组合下，任意 step(1..=10) 与任意上游 `Retry-After`(0..=3600) 的**有效冷却恒 ≤15s**（覆盖 §0.5 缓解的正确性证明）|
| `route_retry.rs` | 不变量满足时（冷却 8s / 预算 30s）⇒ 连续拿到 3 轮等待，`routing_round` 到 4 |
| `route_health.rs` | 上游 `Retry-After: 28` + `retry_after_cooldown_cap=5` ⇒ 冷却取本地值，不被抬到 28 |
| `route_health.rs` | `ConcurrencySaturated` 的 `Retry-After` **不**受新 cap 削减（回归保护） |
| `route_health.rs` | 同类连续失败 8 次 + `max_step=3` ⇒ step 封顶 3；本地与 Redis backend 结果一致 |
| `route_health.rs` | 同 host 3 条路由依次 502 + `shared_host_failure_domain=true` ⇒ 三条冷却都走 EdgeProxy 曲线且 step 不升 |
| `runtime_settings.rs` | 违反 T1.1 不变量的设置组合被拒绝，错误消息含两个具体数字 |
| `capability_probe.rs` | 中文 `参数非法：logprobs` ⇒ hint 返回 `logprobs`；`不支持该参数 top_logprobs` ⇒ 返回 `top_logprobs`（不误匹配 `logprobs`） |
| `capability_probe.rs` | 新增字段表的**顺序**回归：`presence_penalty` 不被 `penalty` 类子串误匹配 |
| `dialect_retry.rs` | 无 `param` + message 含字段名 ⇒ 能提取；code=`1210` + message 无字段名 ⇒ 仍返回 `None`（不放行） |
| `upstream_feedback.rs` | HTTP 500 + 中文 `参数非法` ⇒ `RequestRejected`（**不是** `TransientServer`） |
| `upstream_feedback.rs` | HTTP 502 + 纯 nginx HTML ⇒ 仍是 `EdgeProxyError`（回归保护） |
| `capabilities/resolver.rs` | per-model preset 优先于 per-upstream preset；探测档案优先于两者 |

### 4.2 集成测试（`tests/gateway/`）

1. **`route_exhaustion_budget_invariant.rs`（新增）— 本次报错的端到端复现**
   模拟 3 条同 host 路由、每条 2 个 key（共 6 条物理候选）全部返回
   `502 + Retry-After: 28 + JSON body`，精确复现用户日志：
   - 修复前断言：`routing_round == 1`、`physical_attempt_count == 6`、
     `cooldown_seconds == 28`、`give_up_reason == wait_budget`
     （**这四个断言就是用户贴的那行日志，先让它绿，证明复现成立**）
   - 修复后断言：`cooldown_seconds ≤ 5`、`routing_round ≥ 2`、
     `give_up_reason != wait_budget`；上游在第 2 轮恢复时请求应最终 200
2. **`shared_host_failure_domain.rs`（新增）**
   同 host 多 key，第 1 条 502 后其余两条冷却曲线一致且 step 不升；
   共模 same-host transient 达阈值后走延迟重放并回滚冷却
3. **`last_resort_probe_after_attempts.rs`（新增）**
   本轮有物理尝试且全部瞬态失败 ⇒ 仍能 arm 探测（T2.1），且每请求至多 1 次
4. **扩展 `tests/gateway/dialect_retry.rs`**
   中文 400 触发同路由剥离重试并成功；断言路由**未**进入冷却
5. **扩展 `tests/gateway/chat/routing.rs`**
   断言终态日志/错误 details 含 `give_up_reason`、`cooldown_source`、
   真实的 `remaining_candidates` 与 `continuation_route_count`
6. **回归**：`tests/upstream_retry_after_cap.rs`（T4 的既有语义）、
   `tests/gateway/responses/continuation_escape.rs`（P2-P6）、
   `tests/gateway/chat/half_open_busy_ledger.rs`（T3）全部保持绿

### 4.3 构建与验证命令

```bash
rtk cargo build
rtk cargo clippy --all-targets -- -D warnings
rtk cargo test --lib
rtk cargo test --test gateway
rtk cargo test        # 全量
```

### 4.4 内网部署后验收（24~48h）

```bash
# 1) 放弃原因分布：wait_budget 应从主导降到接近 0
rtk grep -ao 'give_up_reason=[a-z_]*' logs/*.log | sort | uniq -c | sort -rn

# 2) 冷却来源与时长：应集中在 local 且 ≤10s
rtk grep -ao 'cooldown_source=[a-z_]*' logs/*.log | sort | uniq -c
rtk grep -ao 'effective_cooldown_ms=[0-9]*' logs/*.log | sort -t= -k2 -n | tail -20

# 3) 轮数分布：routing_round=1 的耗尽应基本消失
rtk grep -a 'routes_exhausted' logs/*.log | grep -ao 'routing_round=[0-9]*' | sort | uniq -c

# 4) 耗尽总量对比（部署前后同时长窗口）
rtk grep -c 'upstream_routes_exhausted' logs/*.log

# 5) 400 自愈率：dialect strip 重试次数应上升，直抛客户端的 400 应下降
rtk grep -c 'route_action=same_route_retry' logs/*.log
rtk grep -ao 'upstream_error_codes=[^ ]*' logs/*.log | sort | uniq -c | sort -rn | head -20
```

**验收标准**：`give_up_reason=wait_budget` 占比 < 5%；
`routing_round=1` 的 `upstream_routes_exhausted` 基本消失；
耗尽总量下降 ≥ 80%；GLM/Deepseek 的 400 直抛量下降 ≥ 50%。

---

## 5. 风险与回滚

| 风险 | 缓解 |
|------|------|
| 冷却削短后，真实长时间故障被反复重试，放大上游压力 | `max_step` 仍保留 3 级升级；`cooldown_max` 内网设 15s 但公网默认不变；共模断路器仍在 |
| T1.4 把同 host 路由绑成一个故障域，掩盖单 key 的真实问题（如某 key 配额耗尽） | 只对 `TransientServer`/`EdgeProxyError` 生效；`Credentials`/`KeyQuota` 仍走 per-key 冷却（`observe_key_failure_at`，`route_health.rs:1370`） |
| T2.3 截断等待延长了单请求的失败耗时 | 上界仍是 `retry_max_wait`（30s），不超过现状；且失败前多一次真实探测机会 |
| T3.1 中文词表过宽，把真瞬态误判成 `RequestRejected`（不冷却 ⇒ 反复打故障上游） | 词表只加**明确指向参数/字段**的短语，不加 `错误`/`失败`/`异常` 这类泛词；配 §4.1 的双向回归用例 |
| T3.3 剥离字段过多，改变模型行为 | 每个新增字段注释写明安全性理由；`thinking` 明确排除；剥离成功后才写入学习档案（沿用既有机制） |
| T3.4 迁移引入的 schema 变更 | `ADD COLUMN IF NOT EXISTS` + `Option` 字段 + 旧配置零改动可用 |
| §0.5 缓解期把 `ConcurrencySaturated` 的 `Retry-After` 也压到 5s，真实并发受限的上游会被更频繁探测 | 缓解期可接受；T1.2 落地后把 `upstream_retry_after_cap_seconds` 回调至 30，改由新的 `upstream_retry_after_cooldown_cap_seconds` 单独控制健康冷却，`ConcurrencySaturated` 分支明确豁免 |
| 全部开关 | T1.2/T1.3/T1.4/T2.1/T2.2/T2.3 各自带 runtime setting，可单独关闭回退到现有行为 |

---

## 6. 任务回填表（实施后填）

| 任务 | 说明 | commit | 状态 |
|------|------|--------|------|
| §0.5 | 配置级缓解（运维动作，零代码） | — | ✅ |
| T0.1 | 终态日志补 give_up_reason 等 | e6a9ccd2 | ✅ |
| T0.2 | remaining_candidates 真实值 | e6a9ccd2 | ✅ |
| T0.3 | candidate_pass_count 更名 + continuation_route_count | e6a9ccd2 | ✅ |
| T0.4 | 冷却写入日志暴露 source/step | e6a9ccd2 | ✅ |
| T0.5 | 采样开启 error body excerpt（运维动作） | — | ✅ |
| T1.1 | 冷却上界不变量校验 | a6f34c0e | ✅ |
| T1.2 | Retry-After 与本地冷却解耦 | e5d24f73 | ✅ |
| T1.3 | 非半开失败 step 上限 | a6f34c0e | ✅ |
| T1.4 | shared-host 故障域 | 606ab72a | ✅ |
| T2.1 | last-resort 探测 arm 条件放宽 | 606ab72a | ✅ |
| T2.2 | 单 host 共模瞬态延迟重放 | 606ab72a | ✅ |
| T2.3 | alignment 截断等待 + 探测 | 606ab72a | ✅ |
| T3.1 | 中文报错词表对齐 | b4768249 | ✅ |
| T3.2 | correction_for_response 放宽 | b4768249 | ✅ |
| T3.3 | 字段表与剥离白名单补齐 | b4768249 | ✅ |
| T3.4 | per-model dialect preset | b4768249 | ✅ |
| T3.5 | GLM/Deepseek 保守档案预设 | b4768249 | ✅ |
| T4.1 | 内网默认参数表 | `<T4 commit>` | ✅ |
| T4.2 | 排查 runbook | `<T4 commit>` | ✅ |
