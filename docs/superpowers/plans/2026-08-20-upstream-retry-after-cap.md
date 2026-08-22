# upstream_routes_exhausted 根因治理 实施计划（待执行）

> 本文原为「上游 Retry-After 上限参数」单点方案；核查代码后发现 Retry-After 只是**放大器**之一，
> 内网部署“跑一段时间就频繁 upstream_routes_exhausted”的**主因是半开单飞（half-open single-flight）
> 把恢复中的路由降级成 1 并发**。文件名保留以便沿用引用，内容已扩展为完整治理方案。

**Goal:** 消除 `upstream_routes_exhausted` 的结构性成因——恢复中的路由不再被单飞探针独占、
半开占用不再被当成“池子耗尽”、上游夸大的 Retry-After 不再钉死路由、凭证瞬断不再静默吃掉 key，
使长时间运行后的可用性不再单调劣化。

**Architecture:** 四个咽喉点：① `RouteHealthRegistry::reserve` / `route_health_reserve.lua` 的半开占用判定
（加独占窗口）；② 流式提交点 `StreamCommitTracker::semantic_output_observed`（首个语义输出即结算健康）；
③ `AttemptLedger` / `RouteRetryPolicy`（半开占用与真冷却分账、不吃盲重试轮数、终态 retry_after 取值修正）；
④ `route_health_outcome()` 与 `record_route_attempt`（Retry-After 统一封顶）。

**Tech Stack:** Rust / axum 0.8 / Redis Lua / Vue3 + TypeScript（前端仅设置项定义与类型，页面按 group 自动渲染）。

---

## 0. 结论速览：这到底是什么问题

一句话：**任何一条路由只要失败过一次，在它“冷却结束、等待复检”的那段时间里，
整个网关对这条路由只允许 1 个并发请求；其余并发请求会把它当成“不可用”，
当池子里每条路由都进入这个状态时，请求就直接 503 `upstream_routes_exhausted`。**

而“复检请求”在流式场景下会**持续整个 SSE 流的生命周期**（Claude Code / Codex 一次请求 30s–5min），
所以一次瞬断可以让一条路由在几分钟内只服务 1 个请求。

为什么是“跑一段时间才出现”：进程刚起来时健康表是空的，所有路由**没有健康状态就没有半开门禁**，
并发不受限；随着运行时间增长，每条路由迟早都会踩到至少一次 5xx/超时，
从此这条路由每次“失败 → 冷却 → 复检”循环都会把并发压到 1，
再叠加客户端重试放大 → 越跑越频繁。

**同名报错还有第二条完全不同的来源**：每个上游账号的本地并发闸门默认只有 **4**（C6），
打满后同样返回 `upstream_routes_exhausted`（但状态码是 429、`class_counts` 只有
`concurrency_saturated`）。这条改配置就能解决，**排查时必须先用 §1 的判据把两者分开**。

---

## 1. 报错语义速查（同一个 code，三种状态码）

`upstream_routes_exhausted` 由 `terminal_route_failure_error`（`src/server/gateway/errors.rs:79`）产生，
两个不同分支共用同一 code，必须先区分：

| 终态 | HTTP | 含义 | 判据（错误 details / 消息） |
|------|------|------|------|
| `Temporary` + 纯 429 族 | **429** | 全池限流/配额/并发耗尽 | 消息含 `please try again in Ns`；`class_counts` 以 `rate_limited`/`key_quota`/`capacity_unavailable` 为主 |
| `Temporary` + 其他 | **503** | 全池瞬态不可用（5xx/超时/冷却/半开占用） | 同上，`class_counts` 以 `transient_server`/`transport`/`edge_proxy_error` 为主 |
| `MixedRoutesExhausted` | **502** | 各路由失败原因**不同类**（例如一条 401、一条 400、一条 502） | 消息为 `all eligible upstream routes were exhausted`（无 `please try again`） |

**关键判据（现场自证根因，无需改代码）：**

1. `details.physical_attempt_count == 0` 且 `details.cooled_candidate_count > 0`
   → 本次请求**一个上游包都没发出去**，全部被健康门禁挡下。这是本文要治的形态。
2. 消息里的 `please try again in Ns` 中 **N 特别大（几十秒到 300s），但 `class_counts` 只有瞬态类**
   → 几乎可以确定是**半开占用**而非真冷却：
   `live_recovery` 对半开占用返回的是**租约剩余 TTL**
   （`health_state_recovery`，`src/state/route_health.rs:1489-1494`，TTL 默认 300s），
   而不是真实等待时间。真冷却的 N 不会超过 `upstream_transient_route_cooldown_max_seconds`。
3. `details.give_up_reason == "round_cap"` 且 `details.waited_ms` 远小于 30000
   → 轮数上限（默认 3）先于时间预算触顶，请求在 ~3s 内就放弃了。
4. 502 形态（`MixedRoutesExhausted`）且 `class_counts.credentials > 0`
   → 走的是 key 级隔离累积（见 C4），不是冷却问题。
5. **429** 且 `class_counts.concurrency_saturated > 0`、这些条目的 `upstream_status` 为空
   → 根本没发包，是**网关本地并发闸门**挡的（见 C6）。这条纯配置可解，不要往冷却方向查。

---

## 2. 根因分析（均已核对代码）

### C1（主因）半开单飞把恢复中的路由降为 1 并发，且持有到请求结束

链路：

1. 路由失败 → `observe_route_failure_at` 写入 `HealthState`
   （`consecutive_failures` / `last_failure_class` / `cooldown_until`，`route_health.rs:1008`）。
2. 冷却期内：`reserve()` 返回 `Cooling`（`route_health.rs:655-663`）→ 跳过，正常。
3. **冷却到期后状态并不消失**：`reserve()` 走 `reserve_expired_half_open_route`
   （`route_health.rs:845`），给**第一个**请求发半开租约；
   之后所有并发请求命中 `half_open_generation.is_some() && half_open_expires_at > now`
   → 返回 `HalfOpenBusy`（`route_health.rs:688-700`）。
4. gateway 侧把 `HalfOpenBusy` 与 `Cooling` **同分支处理**（`src/server/gateway.rs:6005-6060`）：
   `record_cooled_route_attempt` 后 `continue 'key_candidates`，等同于“这条路由不可用”。
5. 租约什么时候还？——`finish_route_health_permit`。流式路径在
   `StreamCompletionContext::mark_success()`（`gateway.rs:3199`）触发，
   而它由 `finalize_completion()`（`stream.rs:1256`）在**整个 SSE 流结束**时才调用。
   即：**独占时长 = 一次完整请求的时长**（上限为半开 TTL，默认 300s）。
6. 只有这次复检成功才会 `clear()` 清空状态（`clear_route_for_success`，`route_health.rs:1104`），
   路由才恢复全并发。

Redis 后端语义完全一致（`route_health_reserve.lua:20-31` 的 `blocked()`）。

**后果量化**：池内 3 条路由、20 并发、流式请求平均 60s。三条路由各踩一次 502 后，
稳态下最多 3 个请求在跑，其余 17 个请求在 ~3s 内（3 轮 × 1s）拿到 503 `upstream_routes_exhausted`。
客户端重试再灌进来，继续 503 —— 正反馈。

### C2 半开占用与真冷却在账本里不可区分，还吃掉盲重试轮数、并谎报 retry_after

- `record_cooled_route_attempt`（`gateway.rs:724`）对两种情况记录同样的
  `AttemptFailure{class, retry_after}`，`class` 取**上一次失败的类别**，
  于是“路由正忙着复检”被统计成“瞬态服务故障 3 routes” —— 现场看日志会误判成上游真的在挂。
- `RouteRetryPolicy` 对这种轮次照样计入 `max_rounds`（默认 3，`types.rs:101`），
  3 轮 × ~1s 后 `give_up_reason=round_cap` 放弃，**时间预算 30s 只用了 3s**。
- 终态 `retry_after` 取 `live_recovery.half_open_remaining`（可达 300s，`errors.rs:137-140`），
  于是客户端被告知“等 287 秒”，实际上只要等这次复检的首包（通常 1–3s）。
  这条对 codex/Claude Code 这类**按消息里的 `please try again in Ns` 退避**的客户端伤害很大。

### C3 上游 Retry-After 全链路无上限（原方案的问题，仍然要修）

生产实锤（2026-08-20）：K-API 对 qwen3.8-max 返回 429 + `Retry-After: 105`，
`observe_route_failure_at` 的 `cooldown = explicit.max(local)`（`route_health.rs:1055-1059`）
把路由钉死 105s；同池另一条 502 冷却中、第三条是 400 毒路由 → 爆发窗口内全池 100% 失败。

- 解析 `parse_retry_after`（`src/upstream_feedback.rs:222`）无上限（支持整数秒与 HTTP-date）；
- 消费点 6 处全部无封顶：本地路由/key 冷却、Redis Lua 两处、聚合冷却、
  RouteRetryPolicy 等待、下游 Retry-After header / SSE `retry_after_seconds`。
- **死参数实锤**：`upstream_rate_limit_max_retry_after_seconds`（`types.rs:143`，默认 10s，
  env `UPSTREAM_RATE_LIMIT_MAX_RETRY_AFTER_SECONDS`）全仓无消费点。
- 为什么封顶取 30s：`RouteRetryPolicy` 的 `max_wait` 默认就是 30s，
  超过 30s 的 Retry-After 网关本来就不会等（`GiveUpReason::WaitBudget`），
  让它继续参与冷却计算纯属自伤。

### C4 凭证族一击即 15 分钟 key 级隔离（“跑一段时间”的第二条累积曲线）

`401..=403` 一律判为 `Credentials`（`upstream_feedback.rs:472`），
key 级冷却基数 `CREDENTIAL_KEY_BASE = 15min`、上限 `KEY_COOLDOWN_MAX = 1h`
（`route_health.rs:34-35`），且**没有任何运行时设置可调**。
聚合网关过载/WAF/鉴权抖动偶发一次 401，就让这把 key 在该上游上静默消失 15 分钟；
多 key 池被逐步吃空 → 表现为 502 `MixedRoutesExhausted`（见 §1 判据 4）。

### C5 默认参数偏公网

`transient base 10s` / `max 300s` / `max_rounds 3` / 半开 TTL `300s`
对“单聚合网关、瞬断 2–3s”的内网形态整体偏保守（2026-08-13 方案 R5 已记录，未改默认值）。

### C6 本地并发闸门默认只有 4（**纯配置可解，但必须先排除**）

`UpstreamConfig.max_concurrency` 默认 **4**（`src/state/types.rs:919`），
按 **(upstream, key_fingerprint)** 账号计数（本地 `state.rs:3628`；Redis `upstream_reserve.lua:48-51`
——注意 Lua 的并发闸门对普通请求与 hedge **都**生效）。

- 命中后 `try_reserve_upstream_account_request` 返回 `LocalConcurrency`
  → gateway（`gateway.rs:6186-6205`）把它记为 `ConcurrencySaturated` **冷候选**，
  半开租约以 `Cancelled` 归还，**不写任何冷却**——所以它不会像 C1/C4 那样累积，
  但**全池同时命中时终态一样是 `upstream_routes_exhausted`**，且是 429
  （`is_pure_rate_limit_exhaustion` 把 `ConcurrencySaturated` 计入 429 族，`route_attempts.rs:568-581`）。
- 为什么“跑一段时间才出现”：流式请求会把并发槽占满整个流的生命周期
  （30s–5min），Claude Code / Codex 的并行子任务很容易超过 4；
  白天并发爬升后就长期贴着上限跑。
- 与 C1 叠加：并发被压到 4 → 首包变慢 → 触发 hedge（默认开，12s）→ 见 C7。

**澄清（避免改错地方）**：`requests_per_minute`（默认 20）与 5h 配额
**只对 hedge 生效**（本地 `state.rs:3719-3733`、Lua `upstream_reserve.lua:54-63` 都在 `hedge` 分支内），
普通请求不受这两个配额限制。

### C7（放大器）hedge 会额外占用第二条路由的半开槽

`upstream_hedge_enabled` 默认 **true**，首包超过 `upstream_hedge_delay_ms`（默认 12s）
就对下一条候选路由发起 hedge，而 hedge 同样走 `reserve_route_health`
（`src/server/gateway/upstream.rs:697-712`）：

- 若该候选路由有健康状态且冷却已过 → hedge **拿走它的半开租约**，
  在 hedge 结束前，这条路由对其它请求同样是 `HalfOpenBusy`；
- 若该候选正在 `Cooling`/`HalfOpenBusy` → hedge 直接以 `upstream_hedge_route_cooling` 放弃，
  不产生健康损伤（这部分是对的）；
- hedge 落败方的租约在 winner 确定后随 future drop → `RouteOutcome::Cancelled` 归还，释放及时。

即：C1 的独占范围在开启 hedge 后**翻倍**。T1 的独占窗口同时覆盖这条路径，无需单独改动。

### 与既有方案的关系

2026-08-13 Part A 的 A1（请求内 step 抑制）、A2（预算对齐等待）、A3（全冷却 last-resort 探测）
都已落地，方向正确，但**都绕不开 C1**：A3 的 `reserve_route_health_probe`
在遇到已存在的半开租约时直接返回 `HalfOpenBusy`（`route_health.rs:783-789`），
即“正被独占的路由无法被提前探测”，所以自愈路径在本形态下同样不可达。

---

## 3. 立即缓解（不改代码，管理页运行时设置即可）

代码修复落地前，可用现有设置把伤害压下来（均为 immediate 生效）：

| 设置 | 现值 | 建议 | 作用 |
|------|------|------|------|
| `upstream_route_health_half_open_ttl_seconds` | 300 | **60** | 把 C1 的独占上限从 5 分钟压到 1 分钟；也让终态谎报的 retry_after 上限降到 60s |
| `upstream_transient_route_cooldown_base_seconds` | 10 | **3** | 内网瞬断的冷却窗口 |
| `upstream_transient_route_cooldown_max_seconds` | 300 | **60** | 封住指数升级的天花板 |
| `upstream_route_exhaustion_retry_max_rounds` | 3 | **8** | 轮数不再先于 30s 时间预算触顶（C2） |
| `upstream_route_exhaustion_retry_max_wait_ms` | 30000 | 保持 | 时间预算才是真正的约束 |
| `upstream_transient_last_resort_probe_enabled` | true | 保持 | A3 自愈 |

**另外先查一遍并发闸门（C6，多半是它在贡献 429 那一半）：**

| 位置 | 现值 | 建议 |
|------|------|------|
| 每个上游的 `max_concurrency`（上游编辑页） | 4 | 按上游实际承载调到 **16–64**；聚合网关通常远不止 4 |
| `default_upstream_max_concurrency`（运行时设置，仅影响未显式配置的上游） | 见管理页 | 同步调高 |
| `upstream_hedge_delay_ms` | 12000 | 并发紧张时可调大（如 20000）或临时关 `upstream_hedge_enabled`，减少 C7 的额外占用 |

**副作用交代**：把半开 TTL 调到 60s，意味着**超过 60s 的流式请求**在结束时其
`finish` 的租约归属校验（本地 `owns_half_open` / Lua `owns()`）可能已失效，
该次成功不会清状态、该次失败不会写冷却（不会误伤，只是这一次观测被丢弃）。
这是权衡后的止血手段，T1/T2 落地后应把 TTL 调回 300s（届时 TTL 只作为“进程崩溃残留租约”的兜底）。

---

## 4. 目标与不变量

**目标**：稳态下 `physical_attempt_count == 0` 的 `upstream_routes_exhausted` 归零；
一次上游瞬断的池级影响时长从“请求时长/冷却时长”降到“首包时长”。

**不变量（不得破坏）**：

- 不改变错误分类语义：429/503/502 判定、`FailureClass` 映射、common-mode 熔断行为不变。
- 429 族仍然交还客户端（B3 不变量）：`is_pure_client_rate_limit` 不在网关内吸收等待。
- 半开单飞对**仍在冷却中**的路由（A3 提前探测路径）保持严格单飞，不放大对故障上游的压力。
- 不回溯 clamp 存量冷却（避免与 ModelUnsupported 15min–1h 隔离语义混淆）。
- 上游并发上限（`default_upstream_max_concurrency` / 账号并发预留）仍是压力总闸，T1 不绕过它。

---

## 5. 任务清单

### T1（P0）半开独占窗口：恢复中的路由不再被单个请求垄断 —— ✅ commit `589d929`

- [x] RED：`tests/route_health.rs`（6 个新用例）+ `tests/redis_runtime.rs`（1 个新用例，`#[ignore = "requires TEST_REDIS_URL"]`）
  - 路由失败 → 冷却到期 → 第 1 个 `reserve` 拿到半开租约；窗口内第 2 个 `reserve` 仍为 `HalfOpenBusy`；
    窗口过后第 3 个 `reserve` 返回 `Ready`（且 `half_open == false`）。
  - 窗口过后被放行的请求成功时，仍能通过 `same_observation` 清空路由状态。
  - `reserve_route_health_probe`（仍在冷却中的 A3 提前探测）不受窗口影响，保持严格单飞（含 window=0 用例）。
  - 窗口设为 0 → 从不因半开占用拒绝；设为极大值（600000）→ 退化为现状。
  - Redis 后端同套断言（窗口 150ms，双 AppState 并发验证）。
- [x] GREEN：
  - `src/state/types.rs`：`AppConfig.upstream_route_half_open_exclusive_window_ms: u64`
    + `default_upstream_route_half_open_exclusive_window_ms() = 3_000` + Default 赋值 + `#[serde(default)]`。
  - `src/state/runtime_settings.rs`：字段（`#[serde(default)]`）+ 加入
    `IMMEDIATE_RUNTIME_SETTING_FIELDS` + `from_app_config` / `apply_to_app_config`
    + `validate_and_normalize` 范围 `0..=600_000`。
  - `src/main.rs`：`env_u64("UPSTREAM_ROUTE_HALF_OPEN_EXCLUSIVE_WINDOW_MS", 3_000).min(600_000)` + 启动日志。
  - `src/state/route_health.rs`：
    - `HealthState` 增 `half_open_exclusive_until: Option<Instant>`；`clear()` / `release_half_open()` /
      `observe_route_failure_at` / `observe_key_failure_at` / `reapply_concurrency_probe_delay` /
      `reserve()` 租约回收分支一并清理。
    - `RouteHealthRegistry` 增 `half_open_exclusive_window: Duration`
      （`new_with_runtime_tuning` / `update_runtime_tuning` 各增一个参数）。
    - `reserve_expired_half_open_route` / `reserve_expired_half_open_key`：
      置 `half_open_exclusive_until = Some(now + window)`；`reserve_route_health_probe` 置为租约到期
      （严格单飞，见下方偏离说明）。
    - `reserve()` 的 busy 判定两处（key + route）改为 `half_open_exclusive_until > now`；
      `is_active()`（租约存活，用于 owns/清理/`health_state_recovery`）语义不变。
    - `reserve_route_health_probe` 保持用 `is_active()` 判 busy（不变）。
    - `update_runtime_tuning` 对存量 `half_open_exclusive_until` 做上限收紧（immediate 生效）。
  - `src/state/redis_runtime/route_health_reserve.lua`：新增 ARGV[5] `exclusive_window_ms`，
    grant 时 `HSET half_open_exclusive_until_ms`；`blocked()` 第二段改判
    `half_open_exclusive_until_ms > now_ms`（`half_open_expires_at_ms` 仍用于“过期租约可被接管”清理分支）；
    新增 `can_grant()`：窗口过后不再覆盖存活租约（与本地语义对齐，见偏离说明）。
  - `src/state/redis_runtime.rs`：tuning snapshot 增 `route_health_half_open_exclusive_window_ms`，
    reserve 调用点补 `.arg(...)`；`route_health_probe.lua` / `route_health_finish.lua` /
    `route_health_observe.lua` 的 HDEL 清单补 `half_open_exclusive_until_ms`。
  - `src/state.rs`（`:606` 注册表构造 + `update_runtime_tuning` 传播点）：新参数一并下发本地/Redis。
  - 计数断言同步：`tests/runtime_settings.rs` 48→49、`tests/admin_runtime_settings.rs` 49→50。
- [x] 验证：`rtk cargo test --all` 全绿（1668 passed, 84 ignored）；Redis 用例沿用 `#[ignore]` + `TEST_REDIS_URL` 约定。

**实现偏离记录（与方案文字不同处，均为等价或更保守选择）：**
1. 方案未规定 A3 提前探测租约的独占窗口取值；实现选择 `half_open_exclusive_until = 租约到期`（本地与
   `route_health_probe.lua` 一致），使探针持有的路由对普通 reserve 保持“整租约期 busy”的既有严格单飞语义
   （满足硬性不变量 3），而非默认 3s 窗口。
2. Redis `reserve.lua` 增加 `can_grant()` 守卫：窗口过后调用方不带租约放行、不得覆盖仍在存活的探针租约；
   本地 `reserve_expired_half_open_*` 同样在租约存活且窗口已过时不再发新租约。这是本地/Redis 行为一致的
   必要条件，方案未显式列出。
3. `update_runtime_tuning` 会收敛存量 `half_open_exclusive_until`（窗口调小/归零对在途租约立即生效），
   与“immediate 开关、窗口=0 即退回现状”的要求一致；未触碰冷却（不变量 4）。
4. 行号漂移：`reserve()` busy 判定现位于本地 `route_health.rs` 的 `reserve()` 内（原引用 :661-700 已偏移），
   Redis 侧为 `route_health_reserve.lua` 的 `blocked()`；`state.rs` 传播点现约 :2920（update_runtime_settings）。

### T2（P0）早判健康：流式首个语义输出即结算，独占按首包时长而非整流时长 —— ✅ commit `be97c97`

- [x] RED（实际落在 `tests/gateway/chat/half_open_verdict.rs`，注册于 `tests/gateway/chat.rs`）
  - mock 上游：先发一个语义事件，然后 hold 住流 10s 不结束。
    第一个请求作为复检发出后，**第二个并发请求应在首包后立即被放行**（而非等流结束），
    且 `route_health_snapshot` 显示冷却已清空。
  - 首包之后流中断（transport error）→ 记为一次**新的独立失败**（step 从 1 起算），
    而不是半开探测失败（不套 `ROUTE_HALF_OPEN_FAILURE_STEP_CAP`）。
  - 首包之前就失败 → 现状路径不变（半开失败、step 受封顶、A1 请求内抑制生效）。
  - 客户端中途断开（499）→ 仍不归因路由失败（回归 `attribute_route_failure`，`gateway.rs:4131`）。
- [x] GREEN：
  - `src/state/route_health.rs`：`RouteHealthPermit` 增
    ```rust
    pub async fn settle_healthy(&mut self) -> Result<(), RuntimeCoordinationError>
    ```
    行为：以 `RouteOutcome::Success` 结算当前租约（本地 `registry.finish` / Redis `finish_route_health`），
    随后把 backend 置为 `Settled { state, route, key }`（保留标识与后端句柄）。
    - `finish(outcome)` 在 `Settled` 之后：`Success` / `Cancelled` → no-op；
      各失败变体 → 走**无租约** observe（`AppState::observe_route_failure` 对应的
      registry / `route_health_observe.lua` 路径，`state.rs:1599`）。
    - `Drop`（Cancelled）在 `Settled` 之后 → no-op。
    - 注意：Redis `route_health_finish.lua` 有 `committed_result` 幂等标记（KEYS[8]），
      结算后**不得**再用同一 lease 调 finish；无租约 observe 走的是另一个脚本，天然规避。
  - `src/server/gateway.rs`：`StreamCompletionContext::mark_healthy_verdict()`
    —— 取 `route_health_permit` 锁调用 `settle_healthy()`，内部 `AtomicBool` 保证幂等；
    失败只记 `tracing::error!`，不影响请求。
  - `src/server/gateway/stream.rs`：三处 `commit_tracker.observe_json`
    （`stream.rs:652` / `1109` / `1744`）所在的同步上下文只置
    `health_verdict_pending = true`（首次 `semantic_output_observed()` 由 false→true 时）；
    由所在 async 读循环在处理完当前 chunk 后 `await mark_healthy_verdict()` 一次。
  - **不改非流式 JSON 路径**：其 permit 生命周期本就等于一次请求往返，收益小于改动风险。
- [x] 验证：`rtk cargo test --test gateway`（394 通过）、`rtk cargo test --all`（60 套件全绿）、clippy -D warnings、fmt 全绿。

**实现偏离记录（与方案文字不同处，均为等价或更保守选择）：**
1. 方案写"三处 observe_json"（stream.rs:652/:1109/:1744），实际共 **4 处**：除三处外
   `finish_stream`（:1223，canonicalizer 收尾事件）也走 `observe_json`；prefetch 分类器（:652）
   与 finish_stream 都由循环顶部的 `mark_healthy_verdict_if_due` 兜底，drain 路径（:1109，translated 为
   :1744 push_translated_event）在 drain 后立即结算。顺带确认 `stream_commit.rs` 的
   `health_settle_pending` 标志是每次语义事件都置位（非仅首次），幂等由 `take_health_settle_pending` 的
   一次性 swap + `mark_healthy_verdict` 的 atomic 双层保证。
2. 上下文侧的一次性原子由 helper 先 `store(true)` 再调 `mark_healthy_verdict`（其内部 swap(false) 是
   幂等闸）；方案未指定这层闸由谁置位，取"helper 置位"最小改动。
3. 结算后的 permit **放回** `route_health_permit` 槽位（Settled 态），使后续 `mark_failure` /
   `mark_cancelled` / `mark_success` 自然走 Settled 分支：失败变体→无租约 observe、成功/取消→no-op。
   若不留回槽位，结算后 `mark_failure` 会取到 None 而静默丢失失败（实现中发现并修复）。
4. 同一次 coalesced 读里"语义事件 + 错误帧"同时到达时（drain-Err 分支），先结算再 finalize 错误，
   使该失败按"结算后新失败"记账（fresh streak）；方案未显式覆盖此边界。
5. 本地 `settle_healthy` 的 Success 结算后，用 `remove_cleared_route_and_key` 移除已清空的路由/key
   条目，镜像 Redis `route_health_finish.lua` 成功分支的 `clear_state`（DEL）——本地预留零值条目的
   既有行为会使 `route_health_snapshot` 返回 Some(全零)，与 Redis 的 None 不一致；仅 T2 结算路径
   移除，普通 success finish 行为不变。
6. Redis 侧 `settle_healthy` 协调错误时**恢复原 live 租约**（不丢租约），由正常收尾路径兜底；
   方案未指定该错误分支。
7. 上游内部重试/hedge 的 `StreamCompletionContext` 构造点（`upstream.rs:754`）与
   `tests/unit/server/gateway.rs:857` fixture 同样补 `health_verdict_pending` 字段（编译器强制）。

### T3（P0）半开占用与真冷却分账：不吃盲重试轮数、不谎报 retry_after —— ✅ commit `c42cd88`

- [x] RED（实际落在新增文件 `tests/gateway/chat/half_open_busy_ledger.rs`，注册于 `tests/gateway/chat.rs`）
  - mock 上游：第 1 次命中 = 半开探针（只发 role-only delta、永不语义输出 → T2 结算不触发），
    hold 住流 15s；第 2 次命中 = 500 响铃（独占被破坏即测试失败）。
  - 用例 1 `half_open_busy_pool_terminates_with_busy_count_and_honest_retry`：
    全池仅半开占用 → 503 `upstream_routes_exhausted`，`physical_attempt_count=0`，
    `half_open_busy_count=1`，`cooled_candidate_count=1`，`give_up_reason="half_open_busy_cap"`，
    `class_counts.transient_server=1`（不变量 1：分类不动，独立字段区分）；
    消息 `please try again in Ns` 中 N ∈ 1..=60 且 <100（窗口 60s，不是租约 TTL），
    `retry_after_seconds` 与消息一致。
  - 用例 2 `half_open_busy_rounds_do_not_consume_max_rounds`：`max_rounds=1` + busy 上限 3
    → `give_up_reason="half_open_busy_cap"`，`routing_rounds>=4`，`waited_ms>=3000`
    （busy 轮不占普通轮数）。
  - 真冷却路径的既有断言全绿（回归）。
- [x] GREEN：
  - `src/server/gateway/route_attempts.rs`：`AttemptFailure` 增 `half_open_busy: bool`（记录路径默认 false）；
    `AttemptLedger` 增 `half_open_busy_count()` / `is_all_half_open_busy()`
    （非空 + 全部条目带 busy 标记才算 all-busy）。
  - `src/server/gateway.rs`（行号已漂移，实际约 :730/:6041-6159）：`record_cooled_route_attempt` 增
    `half_open_busy: bool` 参数，3 个调用点传值（Cooling=false / HalfOpenBusy=true / ConcurrencySaturated=false）；
    把合并的 `Cooling | HalfOpenBusy` match 臂拆成两个独立臂。
    `decide_with_reason` 调用点（:7667）新增
    `busy_only = round_ledger.attempt_count() == 0 && round_ledger.is_all_half_open_busy()`；
    `route_action = "routes_exhausted"` 日志（:7820）增 `half_open_busy_count`。
  - `src/state/route_health.rs`（实际 :1712/:1730 附近，原 :1486 已偏移）：
    新增 `HealthState::half_open_exclusive_remaining()` 与 `half_open_busy_wait()`
    = `min(剩余独占窗口, 剩余租约).max(HALF_OPEN_BUSY_RETRY)`；
    `health_snapshot()` 与 `health_state_recovery()` 的半开占用分支改报 `half_open_busy_wait`，
    修掉“告诉客户端等 287 秒”的问题（`errors.rs:137-140` 的消费点无需改）。
  - `src/state/redis_runtime/route_health_snapshot.lua`：同步改报
    `min(剩余租约, 剩余独占窗口)`、下限 1000ms（与本地 `half_open_busy_wait` 对齐；
    无 `half_open_exclusive_until_ms` 字段的旧状态退回原租约语义）。响应形状（10 字段）不变。
  - `src/server/gateway/route_retry.rs`：
    - `RouteRetryBudget` 增 `busy_rounds: u32` + `busy_rounds()`；`record_wait()` 对
      `wait.busy` 只增 busy 计数、不推进 `current_round`（debug_assert 双向）；
    - `RouteRetryWait` 增 `busy: bool`（既有构造全部显式 false）；
    - `RouteRetryPolicy` 增 `busy_max_rounds`（`from_sources` 读
      `upstream_route_half_open_busy_max_rounds`；`new_with_full_tuning` 全参数；
      测试 helper `new_with_tuning` 默认 10）；
    - `decide_with_reason` 增 `busy_only: bool` 入参（在 enabled/Temporary/B3 检查之后）：
      busy 分支返回 1s+jitter 等待、`next_round` 不变，受 `busy_max_rounds` 与总时间预算约束；
      超限给 `GiveUpReason::HalfOpenBusyCap`（`as_str() = "half_open_busy_cap"`）。
  - `src/server/gateway/errors.rs`：details 增 `half_open_busy_count`。
  - `src/state/types.rs` / `src/state/runtime_settings.rs` / `src/main.rs`：
    `AppConfig.upstream_route_half_open_busy_max_rounds`（默认 `DEFAULT_UPSTREAM_ROUTE_HALF_OPEN_BUSY_MAX_ROUNDS=10`，
    immediate，`validate_and_normalize` 范围 1..=100；env `UPSTREAM_ROUTE_HALF_OPEN_BUSY_MAX_ROUNDS`, clamp 1..=100）。
    `src/state.rs` 补常量 re-export；计数断言 49→50 / 50→51。
  - dashboard 分类细分按方案留到 T7（`upstream_routes_exhausted` 已单列）。
- [x] 验证：`rtk cargo test --test gateway`（396 通过）、`--test route_health`（43 通过）、
  `--test unit` / `--test runtime_settings` / `--test admin_runtime_settings` / `--test admin_dashboard` 全绿；
  fmt / clippy -D warnings / `cargo test --all` 见 T8 复核。

**实现偏离记录（与方案文字不同处，均为等价或更保守选择）：**
1. RED 放新文件 `tests/gateway/chat/half_open_busy_ledger.rs`（方案写 rate_limits.rs 风格；新文件避免
   把并发语义埋进既有重试测试）。终端请求用**非流式**（流式终态错误走 SSE 事件而非 JSON details）。
2. `health_state_recovery` 锚点从方案 :1486 漂移到实际 :1730；`record_cooled_route_attempt` 锚点
   :724 漂移到 :730。
3. Redis `route_health_snapshot.lua` 必须同步改报 min（方案只写了本地 `health_state_recovery`；
   否则 Redis 后端仍报整租约 TTL，违反“本地/Redis 行为一致”）。旧字段缺失时退回原语义。
4. `is_all_half_open_busy` 要求**全部**条目都带 busy 标记且尝试数为 0 才走 busy 分支；
   混合池（真冷却 + 忙碌）走原路径，`half_open_busy_count` 仍独立上报。
5. busy 轮同时受 `busy_max_rounds` 与**共享总时间预算**（`max_wait`，默认 30s）约束
   （方案未指定时间预算归属；选共享预算，拒绝为 busy 单设时间预算，见 commit trailers）。
6. A3 提前探测（T1 明确租约期整段 busy）的 `half_open_busy_wait` 退化为租约剩余 —— 语义不变。
7. 既有测试 `half_open_busy_reports_remaining_lease_time`（曾断言半开占用报整租约 TTL）按 T3 契约
   改写重命名 `half_open_busy_reports_wait_bounded_by_exclusive_window`（窗口前 = min = 3s；
   窗口后 = 1s 下限）；Redis 同语义用例 `redis_half_open_busy_reports_remaining_dedicated_lease`
   在 min 语义下断言仍成立（2s 租约 < 3s 窗口 → min = 租约）。
8. `HALF_OPEN_BUSY_RETRY` 由 `pub(crate)` 改 `pub` 并经 `state.rs` 的 `pub use route_health` 导出
   （`crate::state` 是 `gateway_core::state` 的 re-export，`pub(crate)` 跨 crate 不可见）。

### T4（P1）上游 Retry-After 统一封顶 —— ✅ commit `fcd897d`

- [x] RED：新增 `tests/upstream_retry_after_cap.rs`（4 用例）
  - 上游 429 带 `Retry-After: 3600` → 终结错误 status 429，`Retry-After` header ≤ cap，
    消息内 `please try again in ≤cap s`；`details.retry_after_seconds ≤ cap`；
  - 失败后管理台反馈快照（`upstream_runtime_snapshots_with_feedback`）`cooldown_remaining ≤ cap`
    （严格，见实现记录 2）；
  - `ConcurrencyFull` 同样被封顶（`local_concurrency_full_retry_after_stays_within_cap`：
    本地准入拒绝 retry_after=1s，cap=1 下终态 429、消息/header ≤1s；用 oneshot 门控的 mock
    上游保证第二个请求一定命中准入拒绝，不依赖时间窗）；
  - cap=1 时消息/header ≤1s，半开/恢复路径不受影响；
  - `runtime_settings_cap_change_applies_to_new_failures_only`：`PUT /api/admin/runtime-settings`
    调低 cap 后对**新失败**立即生效（消息/header ≤ 新 cap），且既有 `mark_upstream_*` 快照冷却
    **不回溯**（`.max()` 保留首个失败的大值）；
  - `tests/runtime_settings.rs` / `tests/admin_runtime_settings.rs` 字段计数 50→51 / 51→52；
    校验用例（0 与 3601 拒绝、1 与 3600 接受）落 `runtime_settings.rs`。
  - 既有两条断言旧契约的用例按新契约改写：`rate_limits.rs` `long_retry_after_returns_immediately_without_second_round`
    （header 由 `==147822` 改 `<=30`）、`routing.rs` `rate_limit_retry_after_cools_the_route_without_waiting_in_request`
    （header 由 `==60` 改 `<=30`；路由 registry 冷却由 `>=58s` 改 `20..=40s`，即本地地板）。
- [x] GREEN：
  - `src/state/types.rs`：`AppConfig.upstream_retry_after_cap_seconds: u64`
    + `default_upstream_retry_after_cap_seconds() = 30` + Default 赋值。
  - `src/state/runtime_settings.rs`：字段 + `IMMEDIATE_RUNTIME_SETTING_FIELDS`
    + `from_app_config` / `apply_to_app_config` + `validate_and_normalize` 范围 `1..=3_600`。
  - `src/main.rs`：`env_u64("UPSTREAM_RETRY_AFTER_CAP_SECONDS", 30).clamp(1, 3_600)`。
  - `src/server/gateway.rs`：新增
    `fn clamp_upstream_retry_after(retry_after: Option<Duration>, cap: Duration) -> Option<Duration>`；
    - `route_health_outcome(error, repeat_within_request, retry_after_cap)`：构造 `RouteOutcome` 前 clamp
      —— 覆盖 4 处调用点 + hedge 路径（`upstream.rs:818`）；
    - `record_route_attempt` → `record_failure_with_status` 入参先 clamp（覆盖聚合冷却、
      终结错误 Retry-After、RouteRetryPolicy 等待、SSE `retry_after_seconds`）；
    - `RouteAttemptContext` 增 `retry_after_cap: Duration`（Copy 不变）。
  - 解析层 `parse_retry_after` 与 Redis Lua **不动**（Rust 入口 clamp 已覆盖全部输入路径）。
  - 实现时 grep 核验发现 **3 处旁路**（见实现记录 1），就地补 clamp；所有向 registry/coordinator
    传 `Some(retry_after)` 的路径现都经过 clamp。
- [x] 验证：`rtk cargo test --test upstream_retry_after_cap --test gateway --test runtime_settings --test admin_runtime_settings`

> **T4 实现记录（2026-08-21，commit 回填于验证后）**
> 1. **咽喉点旁路（比方案多 3 处补丁点）**：grep 核验发现 `TooManyRequests` 与 `ConcurrencyFull`
>    两个专用分支在 `route_health_outcome` **之外**直接构造 `RouteOutcome::RouteFailureWithRetry`，
>    且直接调用 `mark_upstream_rate_limited` / `mark_upstream_concurrency_full`
>    （`gateway.rs:7064/7110/7162/7193` 附近）传入未 clamp 的 `retry_after` —— 这会绕过两个咽喉点，
>    让管理台快照继续显示 3600s。处理：在这两个分支解构后立即 clamp（`record_route_attempt` /
>    `finish_route_health_permit` 因此拿到已 clamp 值）；本地/Redis 并发准入拒绝
>    （`gateway.rs:6288` 附近，`record_cooled_route_attempt` + `ConcurrencyFull`）同样就地 clamp。
>    `mark_upstream_*` 快照的 `cooldown_remaining` 因此能严格 ≤ cap（见 RED 断言）。
> 2. **本地退避地板与「冷却 ≤ cap」的张力**：RateLimited/KeyQuota 的本地退避
>    `DEFAULT_RATE_LIMIT_BASE=30s` 经 `explicit.max(local)` 合并（route_health.rs:1303），
>    首击带 ±20% 抖动 → 路由健康 registry 的冷却可为 24–36s，**不随 cap 缩水**（方案 §9 风险表
>    明言「本地指数退避仍在」）。因此 RED 的 `route_health_snapshot.cooldown_remaining ≤ cap`
>    断言落在「反馈快照」（`mark_upstream_*` 路径，无地板、严格 ≤ cap）上；路由 registry 冷却只
>    断言 ≤ 本地地板 + 余量（cap=30 时仍远小于 3600，证明确实封顶）。
> 3. **终态提示额外 clamp**：`terminal_route_failure_error` 的 `please try again in Ns` /
>    `Retry-After` header 取自 live recovery（含本地地板），要满足「消息/header ≤ cap」必须
>    在终态构造处再 clamp 一次：函数新增 `retry_after_cap: Duration` 参数，Temporary 分支
>    `retry_after = ...min(cap)`；`tests/unit/server/gateway.rs` 8 处调用点传
>    `Duration::from_secs(3600)`（近似禁用语义不变）。非聚合路径的 `last_route_error` 因分支级
>    clamp 已天然 ≤ cap。cap=1 时客户端会按 1s 提前重试而路由仍在地板冷却 → 再吃 429（有界抖动，
>    消息恒 ≤ cap），这是「cap 不信任上游夸大值」的必然取舍，见 commit `Rejected:`。
> 4. 解析层 `parse_retry_after` 与 Redis Lua 确认未动；Redis 侧所有 `retry_after` 均由 Rust
>    咽喉点 clamp 后传入，本地/Redis 行为一致。

### T5（P1）凭证族一击轻惩罚 —— ✅ commit `30893a1`

- [x] RED：实际落 3 处——
  - `tests/gateway/chat/credentials_first_strike.rs`（注册于 `tests/gateway/chat.rs`，2 用例）：
    `single_401_cools_key_for_first_strike_seconds_not_quarter_hour`（单次 401 → key 冷却 ≈ 首击秒数
    48..=72s 抖动窗，非 15min）与 `second_401_escalates_to_key_curve_and_first_strike_is_tunable`
    （`upstream_credentials_first_strike_seconds=2` 时首击 1600..=2400ms；第二次 401 升级
    = 15min-曲线 step2 一半 24..=36min；第三次起不撞 key 配额）。
  - `tests/route_health.rs` 2 单元用例（`credentials_first_strike_cools_short_then_escalates_to_key_curve`、
    `credentials_first_strike_honors_registry_tuning_and_key_quota_unaffected`：registry tuning 即时生效、
    KeyQuota 走 30s 基线与首击无关）。
  - `tests/redis_runtime.rs` `redis_credentials_first_strike_cools_short_then_escalates`（本地无 Redis 则
    ignored，编译进套件）。
  - 字段计数 51→52（`tests/runtime_settings.rs`）/ 52→53（`tests/admin_runtime_settings.rs`）；
    校验用例（0 与 3601 拒绝、1 与 3600 接受）落 `tests/runtime_settings.rs`。
- [x] GREEN：
  - 新设置 `upstream_credentials_first_strike_seconds`（默认 60，范围 1..=3_600，immediate）：
    `src/state/types.rs`（const + AppConfig 字段 + Default）、`src/state/runtime_settings.rs`
    （字段 + `IMMEDIATE_RUNTIME_SETTING_FIELDS` + from/apply + 校验）、`src/main.rs`
    （`env_u64(...).clamp(1, 3_600)`）、`src/state.rs`（re-export + registry ctor +
    `update_runtime_tuning` 第 8 参）、前端项见 T7。
  - `src/state/route_health.rs`：`RouteHealthRegistry` 增 `credentials_first_strike`；
    `key_cooldown`（实际位置已偏移，现约 :1955）`Credentials && step == 1` 用首击值，
    `step >= 2` 沿用 `CREDENTIAL_KEY_BASE` 15min→1h 曲线；`key_cooldown_schedule_ms` 增参；
    `observe_key_failure_at`（现约 :1356）传入 `self.credentials_first_strike`。
  - Redis 侧：`RedisRuntimeTuning` 增字段；`update_runtime_tuning` 第 7 参；两处
    `key_cooldown_schedule_ms` 调用点（finish 路径 ~:1464、`observe_key_failure` ~:1583）传
    tuning snapshot。schedule 由 Rust 侧预计算，**Lua 不动**（与方案一致）。
- [x] 验证：`rtk cargo test --test route_health`（45）、`--test runtime_settings`（28）、
  `--test admin_runtime_settings`（10）、`--test gateway`（398，含新 2 用例）、
  `cargo build --all-targets` 干净；`--test redis_runtime` 编译通过（83 ignored）。

> **T5 实现记录（2026-08-21，commit 回填于验证后）**
> 1. **测试栈回归（与产品行为无关的测试基建修复）**：T5 增加 AppConfig/运行时设置字段后，
>    既有流式测试 `slow_first_output_hedge_uses_the_next_upstream_account` 在 debug 构建下
>    SIGABRT "stack overflow"——复现/二分确认是**二进制布局敏感**的既有深 drop 链
>    （held-open 上游流 + 胜出 hedge 的释放链）在 libtest 2MiB 线程栈上的边界问题，不是
>    T5 语义引入（仅加字段即翻转布局；直跑通过、rtk 沙箱下必现；同族 Responses 用例通过）。
>    处理：测试改为在显式 16MiB 栈线程 + current-thread runtime 上运行
>    （`tests/gateway/common.rs` 新增 `run_on_big_stack`，要求 `Send + 'static`），
>    测试语义零改动。未改任何产品代码。
> 2. 方案锚点 `route_health.rs:1096` 已漂移：`key_cooldown` 实际在 :1955 附近；
>    `observe_key_failure_at` 在 :1356 附近；`update_runtime_tuning` 的 Redis 侧在
>    `redis_runtime.rs :225` 附近（7 个已有序参后追加第 7 参）。
> 3. 未动 `CREDENTIAL_KEY_BASE`（15min）/`KEY_COOLDOWN_MAX`（1h）常量：首击只替换
>    `step == 1` 的基数，`failure_step` 推进逻辑不变，第二次起仍走既有曲线。
> 4. Redis 与本地 schedule 均由 Rust 预计算，两边输入同一 `credentials_first_strike`，
>    行为一致；Lua 无改动。

### T6（P2）清理死参数 —— ✅ commit `3d59447`

- [x] 删除：行号已漂移（实际 `types.rs:163/:298`、`main.rs:119-121`）——字段定义、Default 赋值、
  env 读取一并删除；grep 确认全仓 src/ 无消费点（runtime_settings.rs / state.rs 均无）。
- [x] 测试赋值 4+3 处：方案列的 4 处（`upstream_feedback.rs:860`、`admin_runtime.rs:232`、
  `feedback.rs:232`、`streaming.rs:3220`）之外，`tests/docker.rs` 另有 3 处字符串引用
  （2 处 passthrough env 白名单 `:225/:507` + 1 处 "compose 不得宣传已移除 key" 断言 `:668`），
  一并删除（完整清理，避免白名单误导）。
- [x] 验证：`rtk cargo build --all-targets` 干净；`--test gateway`（398）、`--test docker`（18）全绿；
  fmt/clippy 干净。历史文档（2026-07-18 plan/spec、2026-07-24 plan）保留原样不动。

### T7（P2）前端设置项与部署文档 —— ✅ commit `1933bc5`

- [x] RED：`frontend/src/utils/runtimeSettings.spec.ts` 字段计数 47→51、immediate 34→38；
  `validSettings` 与 `expectedKeys` 同步补 4 键。
- [x] GREEN：
  - `frontend/src/types/index.ts`：`RuntimeSettings` 增 4 字段（置于
    `upstream_route_health_half_open_ttl_seconds` 之后）：
    `upstream_route_half_open_exclusive_window_ms`、`upstream_route_half_open_busy_max_rounds`、
    `upstream_retry_after_cap_seconds`、`upstream_credentials_first_strike_seconds`。
  - `frontend/src/utils/runtimeSettings.ts`：4 个设置项定义（group `routing`「路由策略」，immediate），
    文案与后端范围一致（窗口 0..=600000、busy 轮 1..=100、cap 1..=3600、首击 1..=3600）：
    - 半开独占窗口（毫秒）：路由复检期间独占的最长时间，超过后其它请求可并发进入；0 表示不独占。
    - 半开占用最大轮数：整池都在复检时，请求最多重试的轮数（不占用普通重试轮数）。
    - 上游 Retry-After 上限（秒）：上游 429/503 携带的 Retry-After 超过该值时按该值封顶。
    - 凭证首次失败冷却（秒）：401/403 第一次只短暂隔离 key，连续失败才升级到 15 分钟以上。
  - `Settings.vue` 按 group 自动渲染（`runtimeSettingGroups` + `fieldsForGroup`），确认无需改动。
  - `DEPLOYMENT.md`「Intranet / Aggregated Gateway Deployment」小节：表格补 4 行；
    建议值段补四项默认值与 C6 并发容量测算（`max_concurrency` 默认 4 按 (upstream, key) 计，
    Claude Code / Codex 并行子任务与长流长期占槽，内网聚合网关建议 16–64；
    `requests_per_minute` / 5h 配额只作用于 hedge）；排障指引补 `give_up_reason=half_open_busy_cap`
    + `half_open_busy_count` 判据、`physical_attempt_count=0` 语义、`cooldown_seconds ≤ cap`
    核验口径。
- [x] 验证：`rtk npm --prefix frontend test`（37 文件 / 271 用例全绿）、
  `rtk npm --prefix frontend run type-check`（vue-tsc --noEmit 干净）。

### T8 全量验证与部署

- [x] `rtk cargo fmt --all --check`、`rtk cargo clippy --all-targets -- -D warnings`（均干净）；
  `rtk cargo test --all`（**1689 passed, 87 ignored**, 61 suites）；前端
  `vitest` 271 passed / `vue-tsc --noEmit` 干净。
- [x] `rtk bash scripts/deploy.sh`（不加 `--force-copy-config`）—— 见下方部署记录。
- [x] 部署后检查并调回 §3 止血值（见部署记录 3）。

> **T8 部署记录（2026-08-21）**
> 1. 代码默认值确认：TTL=300s、cooldown base=10/max=300、rounds=3——T1/T2 落地后 TTL 只兜底崩溃残留租约。
> 2. 部署命令：`rtk bash scripts/deploy.sh`（未加 `--force-copy-config`，保留部署目录既有
>    docker-compose.yml / .env）。镜像构建 + compose up + 健康检查 + 前端资源校验由脚本完成，
>    容器 04:19 起 healthy；新设置默认值（窗口 3000 / busy 轮 10 / cap 30 / 首击 60）
>    已在实例上确认生效。
> 3. §3 止血值回退：经管理 API（revision 6→7）把 `upstream_route_health_half_open_ttl_seconds`
>    120→**300**（immediate，无 restart）。冷却 base=2/max=60、rounds=6 与 DEPLOYMENT.md 内网
>    建议一致，按 §7 观测（≥24h）后再调——本记录提交时观测未完成，为待办。

---

## 6. 测试矩阵（并发形态必须覆盖）

现有 gateway 测试多为单请求串行，本方案的核心是并发语义，需要新增并发用例：

| 场景 | 期望 |
|------|------|
| 一条路由失败 → 冷却到期 → 4 个并发请求 | 窗口内 1 个进入、3 个 busy；窗口后其余进入（T1） |
| 复检请求 hold 住流 10s，期间第 2 个请求到达 | 首包后立即放行，冷却已清（T2） |
| 全池半开占用，`max_rounds=1` | 请求仍能等到放行，`give_up_reason` 不是 `round_cap`（T3） |
| 全池半开占用且始终不放行 | `give_up_reason=half_open_busy_cap`，`half_open_busy_count>0`，retry_after ≤ 窗口（T3） |
| 上游 429 + `Retry-After: 3600` | 冷却 ≤ cap，消息 `please try again in ≤cap s`（T4） |
| 单次 401 | key 冷却 ≈ 60s；连续两次 → 15min 起（T5） |
| 回归 | 2026-08-12 共模熔断 7 类用例、2026-08-13 Part A 的 A1/A2/A3/A5 用例全绿 |

Redis 后端：T1（reserve.lua）、T2（settle_healthy 的无租约 observe）必须各跑一遍本地/Redis 双后端。

---

## 7. 观测与验收（部署后如何证明修好了）

1. `route_action=routes_exhausted` 日志中 `physical_attempt_count=0` 的占比应趋近 0；
2. 新字段 `half_open_busy_count` 在稳态下应为 0（出现即说明复检确实慢，看首包延迟）；
3. `give_up_reason` 分布：`round_cap` 应基本消失（改由时间预算或 busy 上限收口）；
4. 终态消息中的 `please try again in Ns`：N 不应再出现接近半开 TTL 的大值；
5. 制造一次大 Retry-After（或等自然 429），确认日志 `cooldown_seconds ≤ cap`；
6. 长时间运行（≥24h）后重复取样，确认 503/502 比例不随运行时长单调上升 —— 这是本方案的最终验收口径。
7. **429 那一支单独看**：`class_counts.concurrency_saturated` 的占比应随 `max_concurrency` 调整而下降；
   若调高后仍高，说明上游真实承载不足（该扩容/加 key），与本方案的健康门禁无关。

---

## 8. 提交规范

每个 Task 一个 commit，trailer 齐全：`Constraint:` / `Rejected:` / `Confidence:` / `Scope-risk:`。
T1/T2/T3 属同一根因族，建议同一分支顺序提交并一起部署（单独上 T2 而不上 T1 收益有限）。

## 9. 风险与回滚

| 任务 | 风险 | 缓解 / 回滚 |
|------|------|------|
| T1 | 独占窗口过后放行的并发会打到仍在故障的上游 | 窗口默认仅 3s；上游并发上限仍生效；失败会立刻重新冷却；设为极大值即退回现状 |
| T2 | 首包成功但随后整流失败的路由，backoff 从 step 1 重算，退避变弱 | 这是有意的语义（已产出语义输出 ≠ 路由不可用）；连续失败仍会重新升级；可用 `upstream_transient_route_cooldown_base_seconds` 调硬 |
| T3 | busy 轮不占 `max_rounds` 可能让请求在网关内多停留几秒 | 仍受 30s 总时间预算与 busy 轮上限双重约束 |
| T4 | 真实需要长退避的配额类 429 被提前重试 → 再吃 429 | 本地指数退避仍在，cap 只是不再信任上游的夸大值；cap 可调到 3600 近似禁用 |
| T5 | 真·失效凭证会被每 60s 重试一次 | 第二次起即升级回 15min→1h 曲线；首击秒数可调大 |

- **Constraint:** 不改变错误分类语义（429/503/502 判定与 `FailureClass` 映射不变）；
  不回溯 clamp 存量冷却；A3 提前探测保持严格单飞。
- **Rejected:**
  ① 在 `parse_retry_after` 解析层 clamp（拿不到运行时设置，签名改动波及全部调用方与测试）；
  ② 直接删除半开单飞（会让故障上游被并发打爆，半开机制本身是对的，问题在于**独占时长无界**）；
  ③ 用“每路由半开在途计数器”替代时间窗口（需要本地+Redis 双份计数与泄漏兜底，复杂度远高于时间窗口，收益相同）；
  ④ 修改 `route_health_finish.lua` 的成功清理语义（幂等标记 KEYS[8] 会与二次结算冲突，改用无租约 observe 更安全）。
- **Confidence:** C1/C2 为代码实锤（可静态复现）；C3 有生产实锤；C4/C5 为累积型推断，
  部署后按 §1 判据可自证。
- **Scope-risk:** high（触碰路由健康核心路径与流式生命周期），因此 T1/T2/T3 各自独立可回滚，
  且都带运行时开关（窗口=极大值 / busy 轮数=1 即可退回现状）。

---

## 10. 关联发现（本次不做，建议另立计划）

1. **RequestRejected(400) 提前终止**：`gateway.rs:7137` 附近，单个上游 400 拒绝载荷后
   `break 'candidate_passes`，健康上游完全不被尝试（生产实锤：某路由 38/38 全败仍稳定吃掉 1/3 流量）。
   建议：仅当全部候选都拒绝（真共模）才终止。
2. **共模熔断同签名误判（内网高危）**：内网多个上游代理同一后端时错误签名完全相同，
   `route_local_fault` 只按 host 区分（`gateway.rs:769` 附近），
   可能被判为全池故障而提前 502。
3. **ModelUnsupported 15min–1h 隔离**（`route_health.rs:36-37`）：
   一次模型名/大小写问题即隔离 15 分钟，与 2026-08-13 Part B 的别名归一相关联。
4. **能力探测降级会悄悄缩小候选池**：`route_is_candidate` 要求
   `route_capability(...).eligible`（`gateway.rs:5446` 附近，`eligible` 定义在 `gateway.rs:179/253`）。
   探测把某能力判成不支持后，该路由在需要该能力的请求上直接不进候选——
   这不会自己产生 `upstream_routes_exhausted`，但会让池子变小、更容易耗尽。
   本次未追（属能力探测子系统，见 2026-08-10 / 2026-08-11 两份方案）；
   排查时若发现 `route_count` 明显小于配置路由数，往这个方向查。

---

# T9（收尾）复核发现的两处修复 + 一处已知缺陷记录 —— ✅ commit `df17cfa7`

> 来源：2026-08-21 对 T1–T8 的**代码级复核**（逐 commit 读实现，非文档回填复核）。
> T1–T8 的语义正确、`cargo test --all` 通过（复核时重跑：全量跑到 doc-tests；
> 单跑 `route_health` 45 passed、`upstream_retry_after_cap` 4 passed、
> 8 个新 gateway 用例全绿）。以下是复核额外发现的问题。

## T9 现状：代码改动已在工作区（未提交、未加测试）

复核过程中已就地改好两处，**工作区脏、尚未提交**，接手者需要补测试与验证后再提交：

```
 M src/server/gateway.rs                          （F1）
 M src/state/route_health.rs                      （F2 本地）
 M src/state/redis_runtime/route_health_probe.lua （F2 Redis）
 M src/state/redis_runtime.rs                     （F2 新增 ARGV）
```

`rtk cargo build --all-targets` 已通过。**接手者请先 `rtk git diff` 读一遍现有改动**，
不要重写，只补测试/文档/验证。

### F1 `mark_healthy_verdict` 的 take→await→insert 竞态（已改）

**问题**：T2 的 `StreamCompletionContext::mark_healthy_verdict`（`gateway.rs:3232` 附近）原实现
把 permit 从 `Arc<TokioMutex<Option<..>>>` 里 `take()` 出来、`await` 结算、再 `get_or_insert` 放回。
在这段窗口内，并发的完成路径（流错误收尾 `mark_failure`、`PreHeaderStreamCancellation`）
会看到**空槽**并静默跳过——那一次失败观测就丢了。

**改法**：改为持锁结算（`lock().await` 后 `as_mut()`），并发路径改为等待而不是看到空槽。
`settle_healthy` 内部只 await registry mutex / Redis 往返，不会再取这把锁，无死锁风险。

### F2 A3 提前探测的独占不再持续整个租约（已改）

**问题**：T1 给 A3 提前探测（`reserve_route_health_probe` / `route_health_probe.lua`）设的独占
是**整个租约期**（`half_open_ttl`，默认 300s）。冷却期内这没问题（其它请求本来就看到 `Cooling`），
但**冷却结束之后**探针仍然独占该路由，直到探针请求结束——首包慢的模型（内网实测就慢）
会让一条本该恢复的路由继续被压到 1 并发，最长 300s。这正是 C1 的尾巴。

**改法**：探针独占改为 `min(now + 租约, max(剩余冷却, now + 独占窗口))`：

- 冷却未结束 → 严格单飞（不变，不给故障上游制造惊群）；
- 冷却结束后 → 退化为普通 3s 独占窗口，与 T1 的常规路径一致；
- 至少保证一个完整窗口（探针在冷却尾声才拿到租约时不会立刻被穿透）。

Redis 侧 `route_health_probe.lua` 新增 ARGV[5] `exclusive_window_ms`，并把 key 级租约的独占
改为普通窗口（镜像本地 `reserve_expired_half_open_key`）。

## T9 待办清单

- [x] RED（F2 本地）：`tests/route_health.rs` 新增
      `early_probe_exclusivity_ends_with_the_cooldown_not_the_lease`：
  - 构造与断言如方案（含边界用例 `early_probe_at_cooldown_tail_keeps_a_full_exclusive_window`）；
  - RED 实测：stash 掉 F2 三文件后两条新用例均失败（旧行为 `HalfOpenBusy`，断言 `Ready` 且
    `!lease.is_half_open()` 处 panic），恢复 F2 后全绿。
- [x] RED（F2 Redis）：`tests/redis_runtime.rs` 新增
      `redis_early_probe_exclusivity_ends_with_the_cooldown_not_the_lease`
      （`#[ignore = "requires TEST_REDIS_URL"]`，用短冷却配置 base=1s/max=2s、窗口 500ms，
      sleep `cooldown_remaining + 700ms` 实等冷却结束；断言顺序：冷却中 `Cooling` → 冷却后
      `Ready` 且 `!is_half_open()`；探针 Success 清健康）。**已对真实 Redis 跑通**
      （`TEST_REDIS_URL=redis://172.18.0.4:6379/0`，1.86s），覆盖 ARGV[5] 新顺序。
- [x] 回归：`half_open_exclusive_window_does_not_affect_early_probe_single_flight`
      （探针 vs 探针单飞）与全部 `half_open_verdict` / `half_open_busy_ledger` / 
      `credentials_first_strike` 用例全绿（`cargo test --all` 1691 passed / 88 ignored）。
- [x] F1 未写竞态用例；以既有 `half_open_verdict` 4 个用例 + `cargo test --all` 回归。
      补充人工核验：`settle_healthy` 只取 `self.backend`（permit 内部字段），不重入 slot 锁；
      `finish_route_health_permit` 在 await 前释放 slot 锁 → 无锁序反转，持锁结算无死锁。
- [x] 验证门：`rtk cargo fmt --all --check`、`rtk cargo clippy --all-targets -- -D warnings`、
      `rtk cargo test --all`（1691 passed, 88 ignored, 61 suites）全绿。
- [x] 提交：一个 commit（trailer 齐全），message 体现 F1+F2 同属 T2/T1 的收尾修复。
      commit hash = `df17cfa7`（见本行上方标题）。

> **T9 实现记录（2026-08-21，复核后补测与提交）**
> 1. **行号与方案快照的漂移**（改动前已 grep 确认）：F1 `mark_healthy_verdict` 实际在
>    `gateway.rs` :3224（结算块 :3242-3257，原快照 :3232）；F2 本地
>    `reserve_route_health_probe` 实际在 `route_health.rs` :971，独占计算块 :1044-1063
>    （原快照 :1041）；`redis_runtime.rs` 的 ARGV[5] 追加在 :1360-1365（probe 调用点，
>    原快照 :1360）；`route_health_probe.lua` ARGV[5] 在 :7，独占计算在 :71-77、
>    key 级普通窗口在 :102-108。
> 2. **F2 语义确认**：本地与 Lua 公式一致 = `min(now+租约, max(剩余冷却, now+窗口))`，
>    并保留「至少一个完整窗口」：冷却尾声拿租约时 `剩余冷却 < 窗口` → 独占 = now+窗口；
>    测试用边界用例覆盖（冷却中 reserve 仍报 `Cooling`——冷却判定先于租约判定，这是顺序
>    不变量，独占的"完整窗口"在冷却结束后以 `HalfOpenBusy` 形式继续体现）。
> 3. **Redis 测试只 finish 探针 permit**：无租约准入的 permit 未写入 Redis（generation 空、
>    lease_id 未入库），finish 会走 finish.lua 的 owns() 失败分支——与既有 Redis 用例
>    （`redis_route_health_probe_ignores_cooldown_and_is_single_flight`）一致，只 finish
>    持有租约的探针 permit，无租约 permit 直接 drop。
> 4. **发现（非 F2 引入，已核对证据）：6 条既有 ignored Redis 用例在真实 Redis 上失败**：
>    `redis_route_health_exclusive_window_allows_admission_after_window`、
>    `redis_settle_healthy_releases_exclusive_window_for_other_state`、
>    `redis_settled_permit_failure_observes_route_without_lease`、
>    `failed_redis_token_recording_does_not_queue_a_duplicate_usage_log`、
>    `redis_coordinated_probe_plan_reserves_and_releases_upstream_capacity`、
>    `redis_downstream_token_replay_preserves_original_score_and_value` +
>    `redis_token_recording_retries_commit_after_response_loss`。
>    证据：F2 stash 前后失败集合完全一致（仅新增本任务的 Redis 用例在 base 失败、F2 后通过）；
>    失败模式为时序敏感（如 observe 的 `explicit.max(local)` 本地地板 ~10s 生效后，
>    用例只 sleep 60ms 便 expect half-open permit → `Cooling 10.036s`）；这些用例
>    `#[ignore]` 且 CI 无 Redis service，从未被真正执行过。**不在 T9 范围**，如需修复应单独立项
>    （把 sleep 改为先读 `cooldown_remaining` 再等）。
> 5. **T10 两项运维配置**（cap 30→60、`max_concurrency` 4→16–64）未动代码，待用户在管理页调整。

## T10（运维，无需改代码）两项配置必须调

1. **`upstream_retry_after_cap_seconds` 30 → 60**：T4 把终态 `please try again in Ns`
   也做了 clamp（含本地冷却剩余）。当前内网 `upstream_transient_route_cooldown_max_seconds=60`
   > cap=30，客户端会提前 30s 重试再吃一个 503。把 cap 抬到与冷却上限一致即可消除。
2. **每个上游的 `max_concurrency` 4 → 16–64**（C6）：这条是 429 那一支
   （`class_counts.concurrency_saturated`）的唯一成因，代码不覆盖。T8 部署记录只回退了半开 TTL，
   未提并发上限，需确认后调整。

> **T10 应用记录（2026-08-21，T9 部署后由管理 API 调整，非代码改动）**
> 1. `upstream_retry_after_cap_seconds` 30 → **60**：经 `PUT /api/admin/runtime-settings`
>    （expected_revision 7 → **8**，source=persisted，immediate 生效，无 restart）。
> 2. 全部 **24** 个上游 `max_concurrency`（原 16 个=4、1 个=5、7 个=10）统一提到 **16**
>    （16–64 区间保守起点）：`PUT /api/admin/upstreams/{id}` 逐个更新，24/24 成功。
>    注意该路由方法是 **PUT**（非 PATCH，PATCH 会 405）。
> 3. T9 已部署：`bash scripts/deploy.sh`（未加 `--force-copy-config`）构建新镜像
>    （含 F1/F2），容器 09:37 重建 healthy；设置经重启保留（revision=8、cap=60、
>    ttl=300、window=3000、busy_rounds=10、first_strike=60、cooldown 2/60、rounds=6）。
>    代码 commit `df17cfa`（T9，见上方）。
> 4. 观察口径沿用 §7：若 `class_counts.concurrency_saturated` 仍高，可在 16–64 区间继续上调；
>    §7.6 的 24h 观测仍在进行中。

## 已知缺陷（本次不修，记录在案）

**key-hedge 胜出时健康归属错位（既有问题，非 T1–T8 引入）**：
key hedge（`send_hedge_stream_attempt`）与 primary **共享同一个 `StreamCommitTracker`**
（`upstream.rs:1020` / `1234`），且 key hedge **没有自己的 `StreamCompletionContext`**——
胜出流的字节可能来自 hedge 的另一把 key，但被结算的是 **primary 路由的健康租约**。
T2 只是让这次结算发生得更早（首包而非流末），没有改变归属语义。

> 复核时曾怀疑 T2 的 settle 标志（tracker 上的 `AtomicBool`）会被“没出过内容的路由”消费，
> 进一步核对后**排除**：settle 只在胜出流的 body 循环里触发，而 `prefetch_first_usable_output`
> 保证胜出方一定产出了 usable output，所以被清冷却的那条路由本身是活的。
> 真正的错位只有上面这一条“归属到 primary 而非胜出 key”。

修法（若要做，需单独立项）：给 key hedge 各自的 `StreamCompletionContext` 与路由健康租约，
胜出方结算自己的路由、落败方 `Cancelled` 归还——涉及 hedge 生命周期，风险高于 T9，建议先观测
`route_id` 与 `selected_upstream_id` 在多 key 上游上的分布再决定。
