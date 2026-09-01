# 内网慢模型下网关自己 429：本地槽位闸门跳过队列

- 日期：2026-08-31
- 状态：**根因 A 已实施并验证完成**（`e7c14bfa` 代码 + `56a933a7` 测试，回填见 §10）；
  **根因 B 与 L4 仍待办**——B 是假设，未验证。
- 范围：A 已改 `src/state.rs` / `src/state/types.rs` / `src/state/runtime_settings.rs` /
  `src/main.rs` / 前端 2 处；B 若成立会再动租约释放路径。
- 报告人现场：内网部署（本项目 + PG + Redis），模型响应慢。
  - 场景 1：GLM-5.2，上游仅 1 个高级账号，并发 4，多人争抢，希望尽量抢到机会。
  - 场景 2：deepseek-v4-flash，上游 7 个账号，各并发 4，希望顺畅。
  - 共同症状：**上游账号并发并没满，网关就直接拒绝，不给上游发请求**；429 直接报错。

## 1. 现象与根因 A（已在源码中确认）

### 1.1 结论先说

`AppState::local_slot_queue_plan`（`src/state.rs:1422-1437`）把**秒**当**毫秒**用。
后果是本地槽位队列的"自适应预算"永久失效，并且对内网慢模型会**永久性地跳过排队、直接快失败 429**，
这条路径**不会向上游发任何请求**——正是报告的症状。

### 1.2 单位错用的证据

两个取样函数返回的是**整秒**（`src/state.rs:7476-7507`）：

```rust
fn hold_p50_seconds(&self, ...) -> Option<u64> { ... Some(holds[mid].as_secs()) }
fn hold_p95_seconds(&self, ...) -> Option<u64> { ... Some(holds[...].as_secs()) }
```

而 `local_slot_queue_plan` 拿它们去和**毫秒**做 clamp / 比较（`src/state.rs:1422-1437`）：

```rust
let floor_ms = config.upstream_account_queue_max_wait_ms.max(1);   // 毫秒，默认 10_000
let Some(p95) = table.hold_p95_seconds(account) else { return (floor_ms, false); };
let p50 = table.hold_p50_seconds(account).unwrap_or(0);            // 秒
let scaled = (p95 as f64 * ADAPTIVE_QUEUE_BUDGET_FACTOR) as u64;   // 秒 × 1.5，仍是秒
let budget = scaled.clamp(floor_ms, ADAPTIVE_QUEUE_BUDGET_CEILING_MS);  // 却按毫秒 clamp
let should_skip = p50 > 0 && p50 > floor_ms.div_ceil(1000);        // 秒 vs 秒（这行单位对）
(budget, should_skip)
```

常量（`src/state.rs:7218,7223`；`src/state/types.rs:319,324`）：

```rust
const ADAPTIVE_QUEUE_BUDGET_FACTOR: f64 = 1.5;
const ADAPTIVE_QUEUE_BUDGET_CEILING_MS: u64 = 60_000;
pub const DEFAULT_UPSTREAM_ACCOUNT_QUEUE_MAX_WAIT_MS: u64 = 10_000;
pub const DEFAULT_UPSTREAM_ACCOUNT_QUEUE_MAX_DEPTH: usize = 16;
```

### 1.3 代入内网慢模型的实际数字

设某账号 p95 hold = 30s、p50 hold = 20s（内网慢模型很常见）：

| 量 | 计算 | 结果 |
| --- | --- | --- |
| `scaled` | `30 × 1.5` | `45`（本意 45 秒，却被当 45 毫秒）|
| `budget` | `clamp(45, 10_000, 60_000)` | **`10_000`** —— 恒等于下限 |
| `should_skip` | `20 > 10_000.div_ceil(1000)` 即 `20 > 10` | **`true`** —— 恒为真 |

两个后果：

1. **自适应预算是死代码**：要让 `scaled` 超过 10_000 需要 `p95 > 6666 秒`（≈1.85 小时），
   现实中不可能。所以 `budget` 永远等于下限，60s 上限永远到不了。
   模型越慢，p95 越大，这个"自适应"越没用——方向恰好反了。
2. **慢模型永久跳过队列**：`should_skip` 只要 p50 hold > 10s（默认下限换算成秒）就恒为真。

第 2 条直接产生报告的症状。消费点在 `src/server/gateway.rs:8798-8809`：

```rust
let (queue_budget_ms, skip_queue) =
    if runtime_settings.upstream_account_queue_adaptive_budget_enabled {
        state.local_slot_queue_plan(account_key)
    } else {
        (runtime_settings.upstream_account_queue_max_wait_ms, false)
    };
if skip_queue {
    tracing::info!(..., "local concurrency queue skipped: median hold exceeds the adaptive budget (E4.2)");
} else if wait_for_local_slot_free(...).await? { ... continue 'routing_rounds; }
// 落到下面的终态快失败
```

`skip_queue == true` 时既不排队也不重试，直接落到
`local_gate_concurrency_saturated_error`（`src/server/gateway.rs:8829` 一带）→ 429 返回客户端，
**全程没有物理上游请求**。

两个开关默认都是开的（`src/state/types.rs:314,332`），所以这条路径开箱即中：

```rust
pub const DEFAULT_UPSTREAM_ACCOUNT_QUEUE_ENABLED: bool = true;
pub const DEFAULT_UPSTREAM_ACCOUNT_QUEUE_ADAPTIVE_BUDGET_ENABLED: bool = true;
```

### 1.4 顺带发现：预算的配置来源不一致

`local_slot_queue_plan` 的 `floor_ms` 读的是 **`self.config`**（`src/state.rs:668` 的静态启动配置），
而非自适应分支读的是 **`runtime_settings.upstream_account_queue_max_wait_ms`**（可热调）。
即：**自适应开着时，热改 `upstream_account_queue_max_wait_ms` 不会改变下限**，运维会以为改了没生效。
修复时应统一为同一来源（倾向运行时设置）。

## 2. 根因 B（**假设，必须先证伪或证实**）：本地租约把账号误判为满

这个闸门只在下面的前置条件下才会走到（`src/server/gateway.rs:8777-8784`）：

```rust
if round_terminal.is_some()
    && round_ledger.is_pure_concurrency_exhaustion()   // 本轮唯一触点是本地预分发闸门，无物理上游请求
    && runtime_settings.upstream_route_exhaustion_retry_enabled
    && runtime_settings.upstream_account_queue_enabled
    && !payload_rejected && !stream_only_final_attempt
```

即：**网关"本地认为"所有候选账号都满了**，才会进到这里。所以"上游并发没满却被拒"还有第二个可能来源：
本地租约表（`local_account_lease_count` / `local_account_stale_lease_count`，`src/state.rs:1335-1358`）
高于上游真实占用——例如租约没被及时释放、`LeaseReleaseGuard`（`src/state.rs:514`）在某些路径漏跑、
或 `upstream_lease_stale_after_ms` 判定过宽，导致陈旧租约仍被计入。

**这只是假设，我没有端到端验证过。** 实施者必须先用下面的方式判定，再决定要不要动：

- 打开闸门日志（`"local concurrency queue skipped"` / `"local concurrency queue hit"`），
  取一次现场 429，记录当时 `local_account_lease_count` 与上游账号真实在跑的请求数；
- 若两者一致 → B 不成立，只修 A；
- 若本地计数明显偏高 → B 成立，与 A 一起修，并把陈旧租约的回收补上。

## 3. 场景 2（7 个账号）不是坏的，别顺手"修"它

`src/server/gateway.rs:8771-8776` 的注释写明：整轮必须是 local-concurrency-only，
**目的就是让多账号场景保留"一个账号满、切到兄弟账号"的回退**，而不是把请求停在满账号后面。
也就是说兄弟账号回退发生在这个闸门**之前**。

所以场景 2 的正确做法是**验证**而不是改造：构造 7 个账号、每个并发 4 的用例，
确认单账号饱和时请求会落到其余账号，只有全部账号都本地判满才会进闸门。
若验证发现回退没生效，那是另一个 bug，**先停下报告**，不要和 A 混在一个改动里。

## 4. 内网配置（根因 A 已修，用新开关即可）

根因 A 已在 `e7c14bfa` 修复，**不再需要靠关自适应来绕**。内网慢模型推荐：

```
upstream_account_queue_skip_when_doomed_enabled = false   # 关键：宁可排队，不要本地直接 429
upstream_account_queue_adaptive_budget_enabled  = true    # 保持开启，预算现在真的会随 p95 变化
upstream_account_queue_max_wait_ms             = 30000    # 下限，按慢模型实测 p95 调
upstream_account_queue_adaptive_budget_ceiling_ms = 180000 # 上限必须 ≥ 上面的下限，否则校验拒绝
upstream_account_queue_max_depth               = 16       # 单账号争抢（场景 1）可适当加大
```

三个新键都可在管理界面热改，**不用重启**。注意 `ceiling_ms < max_wait_ms` 会被校验拒绝
（倒挂区间会让 `u64::clamp` panic，已在 `e7c14bfa` 里挡掉）。

§1.4 那条"调大 `max_wait_ms` 没用"的坑也已随 `e7c14bfa` 修掉——现在自适应路径读运行时设置，热改生效。

## 4.1 怎么判定需要做根因 B

**先看 429 响应体，比翻日志准**。本地闸门的 429 会带 `details`，其中三个字段直接给出判据：

```json
{"error":{"code":"gateway_concurrency_saturated","details":{
  "in_flight": 4, "max_concurrency": 4, "stale_lease_count": 0,
  "physical_attempt_count": 0, "queue_depth": 0, "retry_after_source": "local_gate"
}}}
```

判定规则：

| 观察 | 结论 |
| --- | --- |
| `physical_attempt_count == 0` 且 `retry_after_source == "local_gate"` | 确认是**本地闸门**拒的，上游没被问过——症状对上 |
| `stale_lease_count > 0` | **B 成立**：有陈旧租约被计入 `in_flight`，账号被误判为满 |
| `in_flight == max_concurrency` 但上游侧实际在跑的请求**更少** | **B 成立**：本地租约表偏高 |
| `stale_lease_count == 0` 且 `in_flight` 与上游真实占用一致 | **B 不成立**，只是真的满了，加大 `max_depth` / 下限即可 |

日志侧的对照信号（`grep` 关键字）：

- `local concurrency queue skipped` —— 根因 A 的旧行为。**修复后关掉 skip 开关就不该再出现**；
  若仍出现，说明开关没生效或读到了旧配置。
- `local concurrency queue hit: re-running the routing round` —— 排队**成功**等到槽位，这是修复后期望看到的。
- `leaked_reclaimed_total` / 租约回收相关计数上涨 —— 配合 `stale_lease_count > 0` 佐证 B。

**最省事的一步**：先按 §4 关掉 skip 开关，观察一段时间。
若 429 基本消失 → A 是唯一原因，B 不用做。
若仍有 `physical_attempt_count == 0` 的 429 且 `stale_lease_count > 0` → 再做 B。

## 5. 开发任务

### L1 — 修正单位错用，并统一配置来源 ✅ 已完成（`e7c14bfa`）

在 `local_slot_queue_plan`（`src/state.rs:1422`）里把秒显式换算成毫秒再参与预算与比较。
**返回值必须保持毫秒**（调用方直接当 `queue_budget_ms` 用）。

同时把 `floor_ms` 的来源从静态 `self.config` 改为运行时设置，让热调
`upstream_account_queue_max_wait_ms` 在自适应路径上也生效（见 §1.4）。
传参还是读运行时快照由实施者定，但必须与 `src/server/gateway.rs:8801` 的非自适应分支同源。

### L2 — 让 `should_skip` 可配置 ✅ 已完成（`e7c14bfa`）

**更正我早先的论断**：我一度写"修完单位后 `should_skip` 永不触发、成了死代码"，**这是错的**。
ceiling clamp 会让它退化成一个**上限守卫**，而不是死代码：

- **p95 ≤ 40s**：`budget = max(p95_ms × 1.5, floor) ≥ p95_ms ≥ p50_ms` → 确实永不触发；
- **p95 > 40s**：`budget` 被 clamp 到 `ceiling`（默认 60_000）→ **p50 > 60s 时会触发**。

`src/state.rs` 里原注释犯的是同一个错（断言 `budget ≥ p95 ≥ p50` 恒成立，忽略了 clamp）；
该注释已随 `e7c14bfa` 一并改写。

**更重要的是**：`tests/gateway/upstream_local_gate_fast_fail.rs:602`
（`adaptive_budget_skips_queue_when_median_hold_exceeds_floor`）**刻意固定了 skip 行为**——
floor=2s、中位 hold=3s，断言第 5 个请求本地快失败且上游零命中。
所以 skip **是刻意设计，不是 bug**，只是对内网慢模型有害。

实际做法（按用户决定）：新增开关 `upstream_account_queue_skip_when_doomed_enabled`，
**默认 `true` 保持现状**，内网设 `false` 即可永远排队。另外两个原本硬编码的常量
（factor 1.5 / ceiling 60s）也一并做成设置，符合"参数不要写死、要进网关设置"的要求。

### L3 — 先验证根因 B，再决定是否动租约（前置调查）

按 §2 的方法判定本地租约计数是否高于上游真实占用：

- **B 不成立** → 在提交说明里写明"已验证 B 不成立"，到此为止，只交 L1/L2；
- **B 成立** → 定位租约泄漏点（`LeaseReleaseGuard`（`src/state.rs:514`）、请求取消路径、
  `upstream_lease_stale_after_ms` 判定是否过宽），补上回收，并加测试证明陈旧租约不阻塞准入。

**A 和 B 必须是两个独立提交**：它们是不同的 bug，影响面和回滚粒度都不同。

### L4 — 验证场景 2 的兄弟账号回退（只验证，不改行为）

构造 7 个账号、每个并发 4：打满 1 个账号后，请求应落到其余账号；
只有全部账号都被本地判满，才允许进入本地槽位闸门。

`src/server/gateway.rs:8771-8776` 的注释说明回退发生在闸门**之前**，依赖"整轮是 local-concurrency-only"。
若验证发现回退没生效，那是**第三个 bug**，**先停下报告**，不要夹带修复。

## 6. 测试要求

**基线**：`rtk proxy cargo test` 裸跑应为 **62 套件 / 1851 passed / 0 failed / 99 ignored**，rc=0。

必须新增（完成情况逐条标注）：

- ✅ `local_slot_queue_plan` 的单元测试
  （`tests/upstream_concurrency.rs::adaptive_queue_budget_scales_with_observed_hold_in_milliseconds`）：
  - p95 hold = 30s → 预算 `45_000`（修复前恒为 `10_000`）；
  - p50 hold = 20s → 仍跳过（默认开关 true），关掉开关后不跳过且预算不变；
  - 样本不足（< 2）→ 回落到下限 `10_000`、不跳过。
- ✅ 端到端网关测试
  （`tests/gateway/upstream_local_gate_fast_fail.rs::skip_switched_off_queues_the_overflow_instead_of_local_429`）：
  关掉 skip 后请求排队、**打到上游**（`hits == 9`）、返回 200，而不是本地 429。
- ❌ **未做**：热调 `upstream_account_queue_max_wait_ms` 在自适应路径上生效的**专项测试**。
  代码已改为读运行时设置（`e7c14bfa`），单测里也通过 `update_runtime_settings` 热改了 skip 开关并生效，
  **但没有针对 `max_wait_ms` 热改后下限随之变化的独立断言**。补这条测试是安全的收尾项。

不得破坏：`tests/gateway/chat/rate_limits.rs`、`tests/gateway/capacity_failure_no_cooldown.rs`、
`tests/upstream_concurrency.rs`、`tests/account_concurrency.rs` 全绿；
账号并发不变量（FIFO 恢复、单探测、`max_recovery_probes() == 1`）**一律不得放宽**。

**验证纪律**：

```bash
rtk proxy cargo test > /tmp/verify.log 2>&1
echo "TRUE_RC=$?"
grep -E "^test result:" /tmp/verify.log | awk '{p+=$4; f+=$6; n++} END {printf "套件=%d passed=%d failed=%d\n", n,p,f}'
```

套件数必须等于 62。fmt / clippy / test 各跑一次，各自独立记录退出码，**不要用 `&&` 串联**。
不要 `git add .`，不要 `cargo fmt --all`。

## 7. 验收

1. ✅ 慢模型场景下关掉 skip 开关后不再跳过队列，且**观测到物理上游请求**（端到端测试断言 `hits == 9`）；
2. ✅ 自适应预算随 p95 变化，不再恒等于下限（单测断言 `45_000`）；
3. ⚠️ 热调 `max_wait_ms` 在自适应路径上生效——**代码已改，缺专项测试**（见 §6）；
4. ⏸ 场景 2 兄弟账号回退验证（L4）**未做**；
5. ✅ 全量 62 套件 / 1853 passed / 0 failed；
6. ⏸ 根因 B **未判定**——判定方法见 §4.1，结论须写进后续提交说明。

### 7.1 反目标（做了就算没完成）

- **只调大超时或队列上限**：掩盖单位 bug，不算修复；
- **用关开关代替代码修复**：单位 bug 必须真修（已在 `e7c14bfa` 修掉）。
  注意区分：`upstream_account_queue_skip_when_doomed_enabled = false` 是**内网的正当配置选择**（§4），
  不是"用开关掩盖 bug"——skip 本身是刻意设计，只是不适合慢模型；
- **放宽账号并发断言**：尤其 `max_recovery_probes() == 1`。若测试逼你改断言，
  说明改动破坏了不变量，**先停下报告**。

## 8. 风险与回滚

| 风险 | 说明 | 处置 |
| --- | --- | --- |
| **修完单位后排队真的变长** | 预算现在真的会按 p95 × factor 放大（默认上限 60s），尾延迟变化是预期的 | ceiling 已可配（`..._ceiling_ms`）；嫌长就**显式调低上限**，不要退回坏单位 |
| **关掉 skip 后无望请求会等满预算** | 不再快失败 | 这正是场景 1 要的取舍（宁可等也要抢到机会）。默认 `true` 未变，只有显式关掉才有此行为 |
| **动闸门进入条件破坏场景 2 回退** | 回退依赖"整轮是 local-concurrency-only" | L4 在改动前后各跑一次；进入条件尽量不动 |
| **A 与 B 混在一个提交** | 无法独立回滚 | 强制分成两个提交 |

**回滚**：运行时开关最快——`upstream_account_queue_adaptive_budget_enabled = false` 绕开整条自适应路径
（预算退回静态值、skip 恒 false）。代码回滚 `git revert e7c14bfa 56a933a7`；B 若实施须另开提交，便于独立回滚。

## 9. 我无法从代码判断、需要现场确认的点

1. **内网 p50 hold 是否真的 > 10s**：这是根因 A 生效的前提，取一次 hold 取样百分位确认；
2. **根因 B 是否成立**：见 §2 判定方法；
3. **429 是否全部来自本地闸门**：本地闸门有独立错误码
   （`upstream_local_gate_distinct_error_code_enabled`，`src/state/types.rs:512`）。
   日志里若出现 `"local concurrency queue skipped"`，即证实根因 A 是主因。
   **这是验证 A 最快的一步，建议先做这条再动代码。**

## 10. 回填表（根因 A）

| 任务 | 内容 | commit | 结果 |
| --- | --- | --- | --- |
| L1 | p95/p50 秒→毫秒换算；floor 改读运行时设置 | `e7c14bfa` | ✅ |
| L2 | skip 做成开关（默认 true 保持现状）；factor/ceiling 去硬编码 | `e7c14bfa` | ✅ |
| — | 挡掉 `[floor, ceiling]` 倒挂导致的 `u64::clamp` panic | `e7c14bfa` | ✅ 校验 + 防御性 `max()` |
| — | 单测钉住 `budget == 45_000` | `56a933a7` | ✅ 反向验证：坏单位下得 `10_000` |
| — | 端到端测试：关 skip 后排队并打到上游 | `56a933a7` | ✅ 反向验证：坏单位下复现 `physical_attempt_count: 0` 的本地 429 |
| L3 | 根因 B（陈旧租约） | — | ⏸ **未做**，判定方法见 §4.1 |
| L4 | 场景 2 兄弟账号回退验证 | — | ⏸ **未做** |

### 10.1 验证结果

| 步骤 | 命令 | 退出码 | 结果 |
| --- | --- | --- | --- |
| fmt | `cargo fmt --check` | 0 | ✅ 无差异（只 `-p chat-responses-codex`，未用 `--all`）|
| clippy | `cargo clippy --all-targets` | 0 | ✅ 零 warning / error |
| test | `cargo test` | 0 | ✅ **62 套件 / 1853 passed / 0 failed / 99 ignored**（基线 1851，+2 为本次新增）|

### 10.2 三个新设置的接线点（共 10 处，供 B 或后续加设置时照抄）

| 文件 | 内容 |
| --- | --- |
| `src/state/types.rs` | `DEFAULT_` 常量、`AppConfig` 字段 + `#[serde(default)]`、`Default` impl、`default_*()` 函数 |
| `src/state/runtime_settings.rs` | 键名清单、`RuntimeSettings` 字段、`from_config`、`apply`、`validate`、`use` 导入 |
| `src/state.rs` | 常量再导出（`pub use`）|
| `src/main.rs` | `use` 导入、`env_*` 读取、`AppConfig` 字面量 |
| `frontend/src/types/index.ts` | TS 类型字段 |
| `frontend/src/utils/runtimeSettings.ts` | 管理界面条目（label/control/min/max/unit/description）|
| `tests/runtime_settings.rs` | 计数断言 75 → 78 |
| `tests/admin_runtime_settings.rs` | 计数断言 76 → 79 |

### 10.3 未做项的注意事项

- **L3（根因 B）**：先按 §4.1 判定。`stale_lease_count` 已直接暴露在 429 响应体里
  （`src/server/gateway.rs:1166`），不必翻日志。B 必须与 A **分开提交**。
- **L4（场景 2）**：`src/server/gateway.rs:8771-8776` 的注释说明兄弟账号回退发生在闸门**之前**，
  依赖"整轮是 local-concurrency-only"。这是**验证任务，不要改行为**；若回退没生效，那是另一个 bug，先停下报告。
- **`upstream_account_queue_max_wait_ms` 仍无上限校验**：`e7c14bfa` 挡掉了倒挂 panic，
  但这个键本身只校验了下限 100。若要加上限，注意别和 ceiling 的跨字段校验冲突。
