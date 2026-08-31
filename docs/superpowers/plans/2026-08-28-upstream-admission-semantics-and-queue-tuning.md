# 容量类失败不再冷却路由 —— 让客户端的重试能收敛

- 日期：2026-08-28（**已按 2026-08-28 现场澄清重写；前一版把上游误判为"排队型"，方向错误，整节作废**）
- 状态：待开发（交接给其他模型实现）
- 前序：C1–C7 已全部落地（`6efceb93` C1 / `269cb859` C2 / `4eb8c942` C3 / `24aba924` C4 / `879d1124`+`911a0ca4` C5 / `f8a20bc8` C6 / `a47f2373`+`d3fae88a`+`139a51b1` C7）。C1/C2 把槽位记账修成可信的，是本方案的前提，**不要回退**。本方案改的是**失败处置策略**。

## 1. 现象来源

内网现场（用户原文，按时间顺序）：

> "codex 显示已经 16 分钟了，依旧没有啥返回信息。"
> "我直接[连上游]后，就能正常排队得到响应，结果此项目转换后，感觉变的很慢了。还存在打不到上游的情况。"
> "**上游并发满了，会报错 429 的。claude code 继续尝试，就可以获得成功。是这样的，不是一直会排队，不报 429 的情况。**"
> "目前使用下来，感觉一直没有信息返回出来。一直在等首字。"

**关键事实（第三句，前一版方案误读了这一点）**：上游是**拒绝型**——并发满就回 429；客户端（Claude Code / codex）持续重试**能够成功**。也就是说 **"客户端重试 + 上游 429" 这个组合本身是收敛的、工作正常的**。

问题因此被精确定位为：**同一个重试循环，直连收敛，经过本网关不收敛。**

## 2. 根因：网关把"忙"当成了"坏"

路由健康冷却的设计目的是**把流量从坏掉的路上挪开**。容量类失败（429 / 并发满）不是"这条路坏了"，而是"这条路现在满了，稍后再来"。**对"坏路"冷却有价值；对"忙路"冷却是纯粹的伤害**——它让客户端本来能打中空位的重试，连试的机会都没有。

现在有两条路径都在犯这个错。

### 2.1 上游真 429 ⇒ 路由被冷却 30s，且指数升级

`FailureClass::RateLimited`（上游 429 的归类，`src/upstream_feedback.rs:677`、`:699`）的本地冷却曲线：

```rust
RouteFailureClass::RateLimited | RouteFailureClass::KeyQuota => {
    (DEFAULT_RATE_LIMIT_BASE, ROUTE_COOLDOWN_MAX)     // base = 30s
}
```
`src/state/route_health.rs:2015-2016`（`DEFAULT_RATE_LIMIT_BASE` 定义在 `:35`）

而冷却取值是：

```rust
let cooldown = match (class, retry_after) {
    (RouteFailureClass::ConcurrencySaturated, Some(explicit)) => explicit,
    (_, Some(explicit)) => explicit.max(local),        // ← 30s 的本地曲线盖过上游的 Retry-After
    _ => local,
};
```
`src/state/route_health.rs:1416-1420`

所以即使上游诚实地说"1 秒后再来"，被 `upstream_retry_after_cooldown_cap_seconds`（5s）截断后仍然打不过 30s 的本地曲线 ⇒ **路由冷却 30 秒**。第二次 429 走 step 2（~60s），第三次 ~120s，`jittered_backoff` 指数升级，上限 `ROUTE_COOLDOWN_MAX = 5min`。

**客户端越是正确地重试，网关越是把唯一的路锁得越久。**

代码里已有的注释（`src/server/gateway/route_retry.rs` 的 `client_retryable_rate_limit` 分支）说对了一半：

> "codex honors Retry-After and keeps the task alive, so the gateway must not absorb the cooldown in-process (B3)."

它做到了**不在进程内吸收等待**，但**仍然写了路由冷却**。结果比吸收更糟：客户端 1 秒后回来，网关连上游都不问，直接回 `upstream_routes_exhausted`。

### 2.2 本地并发闸门拒绝 ⇒ 路由被冷却 30s

本地闸门（`src/state.rs:3868` / `:3970` / `:4016` 附近，C1 之后统一为 `table.account_lease_count(&account) >= ...`）拒绝时，gateway 侧记：

```rust
let retry_after = Duration::from_secs(admission_error.retry_after_seconds.max(1))
    .min(upstream_retry_after_cap);                        // cap = 30s
record_cooled_route_attempt(..., FailureClass::ConcurrencySaturated, retry_after, ...);
```

`ConcurrencySaturated` 那条分支让 `explicit` 直接取胜（`route_health.rs:1417`），而 `explicit` 来自估算函数——它测错了对象（见 §2.3），恒为满 TTL，被 30s 上限截断 ⇒ **一次本地拒绝就把路由锁 30 秒**。

glm5.2 只有 1 条上游路由，所以这一次拒绝等于**全局熔断 30 秒**。

### 2.3 Retry-After 估算测错了对象（纯 bug）

```rust
let oldest_remaining = table.oldest_remaining_secs(account, now).unwrap_or(1);
...
oldest_remaining.max(probe_delay_secs)
```
`src/state.rs:7171-7185`（C3.4 引入，`4eb8c942`）

`oldest_remaining` 是**最老租约的剩余 TTL**，不是"预计还要多久服务完"。而 C2 的心跳每 `ttl/3` 就把 TTL 续满（`269cb859`），所以只要请求还活着，这个值恒等于接近满 TTL（300s），再被 `upstream_retry_after_cap_seconds`（30s）截断 ⇒ **给客户端的 Retry-After 恒为 ~30s，与真实等待时间完全无关**。

16 分钟 ÷ 30 秒 ≈ 32 次重试，与现场观察吻合。

### 2.4 三项代价叠加：我们的拒绝比上游的拒绝贵得多

| | 直连上游 | 经过网关 |
| --- | --- | --- |
| 拒绝耗时 | 立刻 429 | 先在 C3 队列里死等 **10s**（`DEFAULT_UPSTREAM_ACCOUNT_QUEUE_MAX_WAIT_MS`，`src/state/types.rs:304`） |
| 告诉客户端等多久 | 上游自己的 Retry-After（通常 ~1s） | **~30s**（§2.3 的 bug） |
| 拒绝后该路由 | 照旧可用，下次立刻能再试 | **冷却 30s 起、指数升级**，期间连试都不试 |

三项都指向同一个后果：**把一个 1 秒粒度就能收敛的重试循环，拖成 30 秒粒度且中途还锁门**。客户端不报错（它认 429、保活、继续等），所以现象只表现为"一直在等首字"。

### 2.5 不是缓冲问题

顺带排除一个常见猜测：流式请求是直通的——

```rust
if downstream_stream {
    return UpstreamAttemptMode::SsePassThrough;
}
```
`src/server/gateway.rs:1431-1432`

网关不攒完再吐，所以"等首字"不是缓冲导致的。

## 3. 架构决策

### 3.1 判据：**容量类失败不写路由冷却**

**决策**：当失败属于"容量类且客户端可重试"（现有概念 `client_retryable_rate_limit` 为真，以及本地闸门的 `ConcurrencySaturated`）时，**只记观测，不写 `cooldown_until`、不推进 `consecutive_failures`**。

**理由**：路由冷却的语义是"这条路不健康，把流量挪走"。容量类失败不携带任何健康信息——上游明确告诉你"我好着呢，只是现在满了"。把它写进健康状态是**语义误用**，后果就是 §2.4。

**必须保留的**：`CapacityUnavailable`、`TransientServer`、`EdgeProxyError`、`ModelUnsupported`、凭据类失败的冷却**一律不变**——那些是真的健康信号。

### 3.2 单路由时一律不冷却（兜底判据）

即使将来有人想给容量类失败保留冷却，也必须加这道兜底：**该 `(runtime_model_slug, protocol)` 当前只有 1 条可用路由时，任何容量类失败都不得冷却它。**

理由：冷却的收益来自"切到别的路"。只有一条路时收益为零，代价是全局熔断。glm5.2 正是这个形态。

### 3.3 本地闸门降级为防雪崩的安全网

**决策**：`max_concurrency` 不再充当"精确匹配上游容量"的主控闸门，而是防雪崩的安全网；**上游自己才是它容量的权威**。

理由：上游会诚实地回 429，客户端会正确地重试，这条链路本来就是收敛的。网关在前面再猜一遍容量，只会引入 §2.4 的三项代价，而且我们的计数天然比上游滞后（租约覆盖整条流 + 异步释放）。

配套：出厂 `default_upstream_max_concurrency()` 从 **4** 提到一个安全网量级的值（建议 **32**，`src/state/types.rs:1164`），并在文档里写清语义变化。

> 注意这与 `2026-08-27` 方案的"4 是正确出厂值"结论相反。那个结论建立在"超过 4 上游就会打挂"的假设上；现场澄清后，正确的结论是**让上游决定**。

### 3.4 本地拒绝要便宜

本地闸门真触发时（撞到安全网），代价必须接近上游自己的 429：

- **不进队列死等**：C3 队列只在"有证据表明很快会有槽位"时才有意义。用 §3.5 的持有时长统计判断：`p50_hold` 远大于队列预算时直接跳过队列，别白等；
- **Retry-After 用真实估算**（§3.5）；
- **不冷却路由**（§3.1 已覆盖）。

### 3.5 Retry-After 与队列预算都必须由观测决定

- 采样每个 `(upstream_id, key_fingerprint)` 最近 N 次租约的**持有时长**（release − reserve），维护 p50 / p95；
- Retry-After ≈ `p50_hold − 最老租约已持有时长`，下限一个探测延迟，按队列位置放大；
- 队列预算 = `clamp(p95_hold × factor, floor, ceiling)`，样本不足时回落静态值；
- **删除 `oldest_remaining_secs` 这个口径**——它测的是 TTL，不是服务时间。

## 4. 开发任务

> E1 是现场止血的关键，一个任务就能解决大部分问题。E5 是"下次别再查半天"的关键。

### E1 — 容量类失败不写路由冷却（**最高优先级**）

- 在路由健康记录的入口区分两类：
  - **容量类**：`RouteFailureClass::ConcurrencySaturated`（本地闸门）、`RateLimited` / `KeyQuota` 中**属于并发/速率类且客户端可重试**的（沿用已有的 `client_retryable_rate_limit` 判定，`src/server/gateway/route_retry.rs`）；
  - **健康类**：其余全部，行为**一字不改**。
- 容量类 ⇒ 只写观测字段（`last_failure_class`、计数器、`last_retry_after_seconds`），**不写 `cooldown_until`、不推进 `consecutive_failures`**；
- 新增开关 `upstream_capacity_failure_cooldown_enabled`（默认 **false** = 新行为）。默认改行为是有意的：旧行为在单路由部署下是有害的，不是可选风格。
- **必须同时覆盖 §2.1 和 §2.2 两条路径**——只改一条，另一条照样锁门。

### E2 — 单路由兜底（§3.2）

- 记录路由健康时，若该 `(runtime_model_slug, protocol)` 的可用路由数为 1，容量类失败**无条件**不冷却，与 E1 的开关无关；
- 这是防止将来有人把 E1 开关打开后又踩同一个坑的护栏。

### E3 — Retry-After 测对对象（纯 bug 修复，**不加开关**）

- 租约表记录 `reserved_at`；release 时算持有时长，按账号维护环形样本（默认 32）+ p50/p95；
- `estimate_local_concurrency_retry_after_seconds`（`src/state.rs:7171-7185`）改用 §3.5 的公式；
- **删除 `oldest_remaining_secs` 口径**；
- 顺带核对：`upstream_retry_after_cap_seconds`（30s）截断的是**给客户端的建议值**，改完后这个值不该再成为主导项。

### E4 — 本地闸门降级为安全网（§3.3 / §3.4）

- `default_upstream_max_concurrency()` 4 → 32（`src/state/types.rs:1164`）；
- C3 队列增加"证据不足就不排队"的判断：`p50_hold > 队列预算` 时直接跳过队列，快速失败（省掉 §2.4 的 10 秒死等）；
- 队列预算按 §3.5 自适应，静态 `upstream_account_queue_max_wait_ms` 降为下限（**不要**直接改成分钟级，那会让注定失败的等待变成分钟级静默）；
- 迁移提示：已有部署的 `max_concurrency` 是持久化值，改默认**不会**自动改现存上游。文档要写清需要用 `POST /api/admin/upstreams/batch-update`（`f8a20bc8`）批量调。

### E5 — 让这类问题下次一眼可见

- **重试放大指标**（最重要）：按 `(downstream_id, model)` 统计单位时间内返回给客户端的 `429 / upstream_routes_exhausted / gateway_concurrency_saturated` 次数。该数远大于真实请求数 ⇒ 客户端正在重试循环。**这次事故没有任何指标能直接指认问题，全靠翻代码，这一条必须做。**
- `ActiveGatewayRequestSnapshot`（`src/state.rs:7273-7289`）新增 `phase`：`selecting` / `queued_local` / `dispatched` / `streaming` / `awaiting_first_output`（+ `queue_position`）。现在只能靠 `upstream_id.is_none()` 反推，分不清"在排队"和"在选路"；
- 上游快照（C5 已有 `in_flight` / `leaked_reclaimed_total` / `stale_reclaimed_total`）新增 `hold_p50_ms` / `hold_p95_ms` / `capacity_reject_total` / `route_cooldown_skipped_total`（E1 生效次数）；
- **路由冷却要能看见来源**：管理端展示每条路由的 `cooldown_until` 与 `last_failure_class`。这次的坑本质是"路由被冷却了但没人看得见"。

### E6 — 首字静默要可见（不改超时）

`upstream_first_semantic_output_timeout_seconds` 默认 **3300 秒（55 分钟）**（`src/state/types.rs:629`）。长推理需要这个预算，**不要贸然调小**：

- 新增 `upstream_first_output_warn_after_seconds`（默认 120）：超过就打 warn，`phase` 置 `awaiting_first_output`（`idle_seconds` 已有，`:7286`）；
- 管理端在途请求列表对超阈值请求高亮。

### E7 — 默认值与文档

- 部署文档新增一节「为什么容量类失败不冷却路由」，把 §3.1 的语义论证写进去，防止将来有人"顺手修回来"；
- 写清 `max_concurrency` 语义变化（主控闸门 → 防雪崩安全网）与批量调整方法；
- 记录本方案与 `2026-08-27` 方案的结论差异及原因（前者假设上游会被打挂，现场澄清为上游会诚实 429 且客户端能收敛）。

## 5. 测试要求

**基线**：实施前自己跑一次 `rtk proxy cargo test` 确认（最近记录 1825 passed，树在动，**不要照抄**）。

**验证纪律**：`rtk proxy cargo test 2>&1 | tail -40` + `echo "TRUE_RC=${PIPESTATUS[0]}"`；统计套件数要重定向到文件再统计，别从 `tail` 结果里数；验证步骤**不用 `&&` 串联**；不要 `git add .`；不要 `cargo fmt --all`。

### 5.1 E1/E2 不冷却（**上线门槛**）

- **单路由 + 上游连续回 429**：客户端每 1 秒重试一次，**每一次都真的被转发到上游**（断言假上游收到 N 次），不出现 `upstream_routes_exhausted`；上游一放开就立刻成功。这条直接对应现场故障；
- **单路由 + 本地闸门连续拒绝**：同上，路由不进冷却；
- 上游 429 后 `cooldown_until` 保持为空、`consecutive_failures` 不增长；
- **健康类失败行为一字不变**：`TransientServer` / `EdgeProxyError` / `CapacityUnavailable` / `ModelUnsupported` / 凭据类的冷却与升级曲线全部与改动前逐字节一致（C1–C7 与 T11 的既有测试必须全绿）；
- `upstream_capacity_failure_cooldown_enabled = true` 时恢复旧行为（回滚路径可用）；
- 多路由场景：容量类失败不冷却也不能破坏跨路由 failover。

### 5.2 E3 Retry-After

- 心跳持续续约的场景下，Retry-After **不再恒为 30s**（钉死 §2.3）；
- 构造已知持有时长样本 ⇒ 估算值落在预期区间；
- 样本不足时回落静态值。

### 5.3 E4 安全网

- 新装默认 `max_concurrency == 32`；
- **已有持久化配置里的 `max_concurrency` 不被改默认值影响**（迁移安全）；
- `p50_hold` 远大于队列预算时**跳过队列**，总耗时不含那 10 秒。

### 5.4 E5/E6 观测

- 重试放大指标：模拟同一 `(downstream, model)` 连续返回 N 次容量类错误 ⇒ 计数为 N；
- `phase` 五态取值正确，`queued_local` 带 `queue_position`；
- `route_cooldown_skipped_total` 随 E1 生效递增；
- 首字超过 `warn_after` ⇒ 打 warn 且 `phase = awaiting_first_output`。

### 5.5 回归

- C1–C7 全部既有测试通过，特别是 `tests/gateway/upstream_concurrency_queue.rs`（`4eb8c942`）与 T11 的冷却曲线测试；
- 本地后端与 Redis 后端结论一致。

## 6. 风险与回滚

| 风险 | 说明 | 处置 |
| --- | --- | --- |
| **容量类不冷却导致无效重试风暴** | 网关不再挡，客户端每秒敲上游 | 这正是直连时已经在发生且工作良好的模式；真需要限流应由**下游 key 的 `per_minute_limit`** 承担，那才是限制客户端的正确位置，不是路由健康 |
| **误把健康类当容量类** | 真坏的路不再被冷却 ⇒ 反复打到坏路 | 分类必须白名单式枚举容量类，其余全部落健康类；5.1 有"健康类一字不变"的回归 |
| **改默认 `max_concurrency` 影响现存部署** | 持久化值不会自动变，运维以为改了 | E4 文档写清 + 提供批量调整命令；5.3 有迁移测试 |
| **429 升级成 key 级长冷却** | `KEY_COOLDOWN_MAX = 60min`（`route_health.rs:38`） | E1 的"不推进 `consecutive_failures`"必须覆盖 key 级路径；测试要断言 key 冷却未被触发 |
| **自适应把注定失败的等待拖长** | 拒绝路径变成分钟级静默 | 自适应只作用于排队，不作用于快速失败；5.3 专测 |
| **改小首字超时误杀长推理** | 55 分钟是为长推理留的 | E6 只加告警不改超时 |

**回滚**：E1 由 `upstream_capacity_failure_cooldown_enabled` 控制（设 true 回到旧行为）；E2 是护栏，不加开关；E3 是 bug 修复，不加开关；E4 的默认值改动只影响新建上游；E5/E6 是纯增量观测。

## 7. 现场止血（不等开发，今天就能做）

按影响排序。**第 2 条是这次的关键**——它直接对冲 §2.1 的 30 秒本地曲线。

| 动作 | 现值 → 建议 | 作用 |
| --- | --- | --- |
| 该上游 `max_concurrency` | 4 → **32** | 本地闸门基本不再拦，请求真打到上游，让上游的 429 成为唯一权威 |
| `upstream_retry_after_cooldown_cap_seconds` | 5 → **1** | 压低上游 429 带来的路由冷却上限。**注意**：它只截断 `explicit` 那一侧，`explicit.max(local)` 里 30s 的本地曲线仍会取胜——所以这条**必须**和下一条一起改 |
| `upstream_retry_after_cap_seconds` | 30 → **5** | 同时压低给客户端的 Retry-After 和本地并发拒绝的冷却值 |
| `upstream_account_queue_max_wait_ms` | 10000 → **2000** | 去掉每次尝试的 10 秒死等 |
| `upstream_first_semantic_output_timeout_seconds` | 3300 → **600**（可选） | 真卡住时 10 分钟就暴露，而不是 55 分钟 |

**坦白一处局限**：只靠配置**无法**把 §2.1 的 30 秒 `DEFAULT_RATE_LIMIT_BASE` 本地曲线压下去——它是编译期常量（`src/state/route_health.rs:35`），没有对应的运行时参数。所以配置只能缓解，**E1 才是根治**。

改完盯三处：

1. `GET /api/admin/troubleshooting/active-requests` —— `upstream_id` 是否还长期为 `null`、`request_id` 是否还在频繁更替；
2. `GET /api/admin/upstreams` —— 路由是否还在冷却、`in_flight` 是否能超过 4（能超过说明闸门确实让路了）；
3. 上游 429 是否升级成了 key 级长冷却（`KEY_COOLDOWN_MAX` 是 60 分钟，真发生了比现在更糟）。

## 8. 任务回填表

> 逐行回填 commit hash 与结果，通过打 ✅，未做写明原因。**不要提前打 ✅。**

| 任务 | 内容 | commit | 结果 |
| --- | --- | --- | --- |
| E1.1 | 容量类 / 健康类分类（白名单式枚举容量类） | `dfa92a1c` | ✅ E1+E2 合并提交 |
| E1.2 | 容量类不写 `cooldown_until`、不推进 `consecutive_failures`（含 key 级路径） | `dfa92a1c` | ✅ 内存 + Redis 后端行为一致 |
| E1.3 | 覆盖本地闸门与上游 429 **两条**路径 | `dfa92a1c` | ✅ 本地闸门走 `Cancelled` permit（只记 ledger），上游 429 走 `is_capacity_class` 观测 |
| E1.4 | `upstream_capacity_failure_cooldown_enabled`（默认 false） | `dfa92a1c` | ✅ 含前端设置页标签 |
| E2 | 单路由兜底：无条件不冷却 | `dfa92a1c` | ✅ `capacity_sole_route`，只影响容量类豁免，健康类曲线一字不变 |
| E3.1 | 租约持有时长采样 + p50/p95 | `c61ac55` | ✅ 每 lease 记录 `reserved_at`，release 采样进每账号 32 样本环形桶 |
| E3.2 | Retry-After 改用持有时长估算，删除 `oldest_remaining_secs` 口径 | `c61ac55` | ✅ `p50 − 最老已持有时长`，样本不足回落 probe 地板；`oldest_remaining_secs` 已删除 |
| E4.1 | `default_upstream_max_concurrency` 4 → 32 | `478dae0` | ✅ 仅影响新建上游；迁移安全测试断言持久化 `max_concurrency=4` 不被动（serde round-trip） |
| E4.2 | 证据不足就跳过队列 + 队列预算自适应 | `478dae0` | ✅ 新增 `upstream_account_queue_adaptive_budget_enabled`（默认 true，前端+Redis 同步）；预算 `clamp(p95×1.5, floor=upstream_account_queue_max_wait_ms, 60s)`；p50 超过静态 floor 时跳过队列直接快速失败。跳过判据用静态 floor（p95 派生预算恒 ≥ p50，不可触发） |
| E5.1 | **重试放大指标** | `42b73aeb` | ✅ 按 (downstream_id, model) 窗口计数；`GET /api/admin/retry-amplification` |
| E5.2 | `phase` 五态 + `queue_position` | `42b73aeb` | ✅ selecting/queued_local/dispatched/streaming/awaiting_first_output |
| E5.3 | 上游快照 hold_p50/p95、capacity_reject_total、route_cooldown_skipped_total | `42b73aeb` | ✅ 内存 + Redis 后端一致（快照 parser 同步补 4 字段） |
| E5.4 | 管理端展示路由 `cooldown_until` + `last_failure_class` | `42b73aeb` | ✅ `route_health_detail_snapshots()` + `admin_list_upstreams` 逐路由挂数组 |
| E6 | 首字告警阈值 + phase 高亮（**不改 55 分钟超时**） | `ef946408` | ✅ `upstream_first_output_warn_after_seconds=120`（immediate）+ 三处流路径 warn + 前端设置页/在途列表/高亮 |
| E7 | 默认值 + 部署文档（含与 2026-08-27 方案的结论差异说明） | `cd0d22a1` | ✅ DEPLOYMENT.md Intranet 小节 + §3.1 语义论证 + batch-update 方法 |

### 8.1 验证结果回填

| 步骤 | 命令 | 退出码 | 结果 |
| --- | --- | --- | --- |
| fmt | `rtk proxy cargo fmt --check` | 0 | ✅（最终全量） |
| clippy | `rtk proxy cargo clippy --all-targets -D warnings` | 0 | ✅（最终全量） |
| test | `rtk proxy cargo test` | 0 | 62 套件 / 1841 passed / 0 failed / 91 ignored（E7 后最终） |
| redis | `TEST_REDIS_URL=… cargo test --test redis_runtime -- --ignored` | — | 未执行（本地无 Redis） |
| frontend | `npm test` + `npm run type-check` + `npm run build` | 0 | ✅ 271 passed / vue-tsc 0 / vite build 0 |

### 8.2 现场验证（内网）

| 项 | 期望 | 实测 |
| --- | --- | --- |
| codex / claude code 长请求 | 不再出现十几分钟无响应 | |
| 客户端重试 | 每次重试都真的被转发到上游 | |
| 路由冷却 | 容量类失败后不再进入冷却 | |
| 重试放大指标 | 接近 1 | |
| `in_flight` | 能反映真实在途（可超过 4） | |
| key 级冷却 | 不出现 | |
