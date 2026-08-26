# 方案：出厂默认配置违反 T1.1 不变量（含升级期设置丢失）+ 生产默认路径覆盖缺口

日期：2026-08-26
状态：待开发
关联：`2026-08-25-route-exhaustion-cooldown-budget-invariant.md`（T0–T4，20/20 已实施）、
`2026-08-25-tool-call-identity-and-anomaly-dimensions-patch.md`（P2.6 已合入 `4aa942da`）

---

## 0. 结论先行

上一轮 T0–T4 **实现层面合格**：20 个任务都是真代码，`cargo fmt` / `clippy --all-targets --all-features -D warnings` / `cargo test` 全绿，
全量 **1777 passed / 0 failed / 88 ignored（62 suites, 264.27s）**，比上一轮基线 1749 多 28 个，与 28 个新增测试函数一一对应。

但"是否满足所有要求"还差三块，其中第一块是**要改代码的真 bug**：

> **P0：出厂默认配置过不了它自己新加的 T1.1 校验。** `AppConfig::default()` 的有效冷却上界是 40s，
> 轮间等待预算是 30s，`40_000 ≥ 30_000` ⇒ `validate_and_normalize()` 直接返回 `Err`。
> **这是我上一轮方案写漏的（T1.1 只规定了不变量，没同步下调出厂默认值），不是实施做漏的**——
> 实施方甚至在测试里明确写了注释说"默认配置故意违反不变量"，说明他发现了却当成了预期行为。

P1 是**生产默认路径零覆盖**：三个默认 `true` 的开关在测试里出现 0 次 `: true`，
新增的 T2.1 探测臂没有正向测试，反而在 8 处既有测试里被关掉当回归保护。
内网真正要跑的那套配置（全默认 ON）端到端没测过。

P2 是 Redis 后端的 T1.2/T1.3/T1.4 从未执行（85 个 `#[ignore]`），`cooldown_source` 断言 0 次。

---

## 1. 报错来源

无新增线上报错。本轮来源是对 `2026-08-25-route-exhaustion-cooldown-budget-invariant.md` 全部 20 项任务的复审，
外加一次针对出厂默认值的实测探针（临时 `tests/tmp_default_invariant_probe.rs`，跑完即删）：

```
base=10 max_step=3 retry_after_cooldown_cap=5 cooldown_max=300 retry_max_wait_ms=30000
ceiling_seconds=40
RESULT: shipped defaults REJECTED -> 违反冷却上界不变量：有效冷却上界 40 秒…必须严格小于轮间等待预算 30000ms
```

---

## 2. 根因

### 2.1 P0：默认值与不变量互相矛盾（数学确定，非概率）

| # | 事实 | 证据 |
|---|------|------|
| 1 | 出厂 `base = 10` | `src/state/types.rs:92` |
| 2 | 出厂 `max_step = 3` | `src/state/types.rs:102` |
| 3 | 出厂 `cooldown_max = 300` | `src/state/types.rs:95` |
| 4 | 出厂 `retry_after_cooldown_cap = 5` | `src/state/types.rs:137` |
| 5 | 出厂 `retry_max_wait_ms = 30_000` | `src/state/types.rs:115` |
| 6 | 有效上界 = `max(5, 10 << 2 = 40).min(300)` = **40s** | `src/state/runtime_settings.rs:757-766` |
| 7 | `40 * 1000 ≥ 30_000` ⇒ 返回 `Err` | `src/state/runtime_settings.rs:725-747` |
| 8 | 实施方自己记录了这个矛盾并绕开 | `tests/runtime_settings.rs:11-21` `compliant_config()` 注释："`AppConfig::default()`（base=10, max_step=3 => 40s ceiling）**intentionally** violates the invariant" |

**为什么全量 1777 个测试没有一个红：** 没有任何测试断言过
`RuntimeSettings::from_app_config(&AppConfig::default()).validate_and_normalize().is_ok()`。
`tests/runtime_settings.rs` 的 20 处 `AppConfig::default()` 全部走 `compliant_config()`（把 base 改成 2）或只测单字段往返，
校验器从未在出厂默认值上被调用过。

### 2.2 P0 的三个后果，其中第二个是升级期静默数据丢失

**后果一（噪声 + 行为与文档不符）：** 每次默认启动都打一条 ERROR 并把预算翻倍到 60_000ms。
`src/main.rs:355-370` 不 panic（内网可用性优先，方向正确），但代价是"出厂即告警"，
且实际生效的轮间预算是 60s 而非文档写的 30s。

**后果二（真 bug）：** `src/state.rs:641-655` 在校验失败时打一条
`"ignoring invalid persisted runtime settings"` 然后**丢弃整份持久化文档**，回退到 `startup_settings`。
叠加两个事实：

- `RUNTIME_SETTINGS_SCHEMA_VERSION` 仍为 `1`，**没有 bump**（`src/state/runtime_settings.rs:27`），
  所以旧文档不会被版本过滤挡掉，会真的进入校验；
- `upstream_transient_route_cooldown_max_step` 带 `#[serde(default)]` = 3（`src/state/types.rs:304-305`），
  旧文档里没有这个 key，反序列化后就是 3。

⇒ **任何在本轮之前通过 Admin 存过运行时设置、且冷却 base 为默认 10 的部署，
下次启动会把运维历史上改过的每一项运行时设置静默回退**，只留一行 error 日志。
这正是当前内网部署的形态（一直在用 Admin 调参）。

**后果三（可用性）：** `src/state.rs:2899` `update_runtime_settings` 调 `validate_and_normalize()?`，
所以运维在 Admin 里改任何一个无关字段点保存都会被拒，报错信息讲的是他没碰过的三个冷却参数。

### 2.3 P1：生产默认路径零覆盖

| 开关 | 出厂默认 | 测试里 `: true` 出现次数 |
|------|----------|--------------------------|
| `upstream_shared_host_failure_domain_enabled` | `true`（`types.rs:1146`） | **0** |
| `upstream_common_mode_same_host_transient_enabled` | `true`（`types.rs:1150`） | **0** |
| `upstream_transient_last_resort_probe_enabled` | `true`（`types.rs:1162`） | **0** |

- 新增集成套件的 harness 在 `tests/gateway/route_exhaustion_budget_invariant.rs:150-185` 把这三个显式设为 `false`；
- `tests/gateway/chat/routing.rs:1001-1018` 把 T2.1/T2.3 关掉当回归保护（注释诚实：「6 hits vs 4」「turn the round-cap 503 into a success」）；
- T2.1 新增臂（`src/server/gateway.rs:8232-8252`）**没有任何正向集成测试**。

上一轮方案 §4.2 点名要的三件东西缺席：

| §4.2 项 | 要求 | 现状 |
|---------|------|------|
| 2 | `tests/gateway/shared_host_failure_domain.rs` | 文件不存在 |
| 3 | `tests/gateway/last_resort_probe_after_attempts.rs` | 文件不存在 |
| 4 | `tests/gateway/dialect_retry.rs` 补中文 400 → 同路由剥离重试 e2e，且断言**路由未被冷却** | 文件 mtime 仍是 8/18，本轮完全未改 |

§4.2-4 那条不是可选项：方言 400 若消耗路由健康，N 条路由 × 1 个不支持的字段 = 立即耗尽，
这是 T3.1 对耗尽问题的直接贡献路径。

### 2.4 P2：Redis 后端未执行 + 一个可观测字段零断言

- `tests/redis_runtime.rs` 有 **85 个 `#[ignore]`**（全量 88 个 ignored 里的绝大多数），
  T1.2/T1.3/T1.4 在 Lua 侧的串接改动**一次都没跑过**；
- `cooldown_source`（T0.4 新增）在 `tests/` 里断言 **0 次**。

### 2.5 P3：调试脚手架

6 处 `GATEWAY_RETRY_TRACE` 门控的 `eprintln!`，全部由本轮 `606ab72a` 引入：
`src/server/gateway.rs:8057`、`:8222`，`src/server/gateway/route_retry.rs:252`、`:369`、`:427`、`:449`。

---

## 3. 开发任务

### P0.1 出厂默认值满足 T1.1（含编译期硬保证）

`src/state/types.rs`：

- `DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_BASE_SECONDS`：`10 → 5`
- `DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_STEP`：`3 → 2`

新有效上界 = `max(5, 5 << 1 = 10).min(300)` = **10s**，`10_000 < 30_000`，留 3× 余量。
冷却曲线由 `10/20/40` 变为 `5/10`。

选这一组而不是别的原因：

| 方案 | 上界 | 余量 | 取舍 |
|------|------|------|------|
| 抬 `retry_max_wait_ms` 到 45s+ | 40s | 1.1× | 客户端在网关内最长等 45s 才拿 503，尾延迟更差，且与"预算存在的目的"相悖 |
| 只把 `max_step` 降到 2（base 保持 10） | 20s | 1.5× | 余量偏紧：同路由重试已先吃掉一部分预算 |
| **base=5 + max_step=2** | **10s** | **3×** | 共模聚合网关本就该快速重探；与 T4 内网参数表想要的低 base 方向一致 |

同时加**编译期断言**，让这类自相矛盾无法再被引入：

```rust
// T1.1: the shipped defaults must satisfy the cooldown-ceiling invariant by
// construction — a default configuration that its own validator rejects makes
// every default boot log an error, blocks Admin saves, and (before P0.2)
// discarded the operator's persisted settings on upgrade.
const _: () = {
    let curve = DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_BASE_SECONDS
        << (DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_STEP - 1);
    let ceiling = if DEFAULT_UPSTREAM_RETRY_AFTER_COOLDOWN_CAP_SECONDS > curve {
        DEFAULT_UPSTREAM_RETRY_AFTER_COOLDOWN_CAP_SECONDS
    } else {
        curve
    };
    let ceiling = if ceiling > DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_SECONDS {
        DEFAULT_UPSTREAM_TRANSIENT_ROUTE_COOLDOWN_MAX_SECONDS
    } else {
        ceiling
    };
    assert!(
        ceiling * 1_000 < DEFAULT_UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_WAIT_MS,
        "shipped defaults violate the T1.1 cooldown-ceiling invariant"
    );
};
```

### P0.2 持久化设置由「整份丢弃」改为「先自愈再校验」

在 `src/state/runtime_settings.rs` 新增一个把 T1.1 修正策略收敛到一处的方法：

```rust
/// T1.1 repair: raise the intra-gateway retry wait budget to `ceiling * 1.5`
/// so the cooldown-ceiling invariant holds.  Returns the corrected budget when
/// a correction was applied.  Single source of truth shared by the startup
/// path (`main.rs`) and the persisted-settings load path (`state.rs`).
pub fn repair_cooldown_ceiling_invariant(&mut self) -> Option<u64>
```

- `src/main.rs:355-370` 改为调用它（行为不变，去掉重复策略）；
- `src/state.rs:641-655` 改为：校验失败 → 克隆 → `repair_cooldown_ceiling_invariant()` → 再校验一次；
  成功则采用并打 `auto_corrected = true` 的 error 日志（说明哪一项被改、改成多少）；
  仍失败才退回现有的整份丢弃。

这样运维历史上存过的其它设置在升级时不再被连带清空。

**不 bump `RUNTIME_SETTINGS_SCHEMA_VERSION`**：bump 会让旧文档在 `state.rs:644` 直接被版本过滤丢掉，
数据丢失照样发生，只是变成"故意的"。自愈路径才是对的修法。

### P0.3 缺失的回归测试

`tests/runtime_settings.rs`：

1. `shipped_default_config_satisfies_cooldown_ceiling_invariant`——
   `RuntimeSettings::from_app_config(&AppConfig::default()).validate_and_normalize()` 必须 `Ok`，
   并断言 `effective_cooldown_ceiling_seconds() * 1000 < upstream_route_exhaustion_retry_max_wait_ms`；
2. `persisted_settings_violating_cooldown_ceiling_are_repaired_not_discarded`——
   持久化一份 base=10/max_step=3 的旧文档，同时把某个无关字段改成非默认值，
   重新加载后断言：那个无关字段**保留**，且 `retry_max_wait_ms` 被抬到 `ceiling * 1.5`；
3. 顺带把 `compliant_config()` 的注释改对（现在出厂默认值本身就合规，注释里"故意违反"的说法必须删掉，
   否则下一个读代码的人会重新引入这个 bug）。

### P1.1 `tests/gateway/shared_host_failure_domain.rs`（§4.2-2）

全默认 ON。同一 host 上 2 个候选、transient 502：

- 冷却走 EdgeProxy 曲线（3s–15s 区间）而非本地指数曲线；
- 失败 step **不升级**；
- 日志里 `cooldown_source` 断言到位（同时补上 P2 的零断言缺口）；
- 对照组：2 个候选在**不同** host ⇒ 正常曲线 + step 升级。

### P1.2 `tests/gateway/last_resort_probe_after_attempts.rs`（§4.2-3）

全默认 ON。N 个候选在第 1 轮全部 502 ⇒ `available_candidate_count() == 0` ⇒

- 恰好一次 last-resort 探测，`last_resort_probe_attempted = true`；
- 每请求最多一次（第二轮不再探）；
- 对照组：`upstream_route_exhaustion_retry_enabled = false` ⇒ 不探测。

### P1.3 出厂默认值端到端回归（新增，覆盖原始线上事故）

用**未经任何覆盖的 `AppConfig::default()`**：6 条逻辑路由挂在同一个 host 上，全部 502 且带 `Retry-After: 28`：

- 请求至少完成一次轮间等待（`routing_round >= 2`）；
- `give_up_reason != "wait_budget"`；
- `distinct_upstream_hosts == 1`；
- 终态 503 的 `remaining_candidates` 是真实值。

这条是把线上事故本身钉在**生产默认配置**上的回归测试；现有套件全部跑在关掉开关的配置上，等于没覆盖事故现场。

### P1.4 `tests/gateway/dialect_retry.rs` 补中文 400（§4.2-4）

- 中文 400（GLM 数字码 `1210` + 字段名）⇒ 同路由剥离字段后重试成功；
- **断言该路由未被冷却**（健康注册表里仍可用、无冷却事件）。

### P2 Redis 后端与可观测字段

- 提供并实际执行 Redis 套件：`REDIS_URL=… rtk cargo test --test redis_runtime -- --ignored`，
  把通过数记进 §6 回填表；本地无 Redis 时必须在回填表里写明"未执行"，不得留空当成通过；
- 增加一个**不需要 Redis** 的测试：断言 Lua 脚本文本/参数个数确实串接了 T1.2/T1.3/T1.4 三个新参数；
- `cooldown_source` 的断言并入 P1.1。

### P3 清理调试脚手架

6 处 `GATEWAY_RETRY_TRACE` + `eprintln!` 改为 `tracing::debug!`（受 `RUST_LOG` 统一控制），
或直接删除。完成标准：全仓 `GATEWAY_RETRY_TRACE` 与相关 `eprintln!` 均为 0 处。

### P4 common-mode 闩锁把 T0 的 `error.details` 全部丢掉（**新发现，本轮实施中挖出，未修**）

这是本轮写 P1.2 测试时暴露出来的**真实缺陷**，不是测试问题。触发条件恰好就是本项目的部署形态
（单聚合网关、多 key、同一 host），也就是 T0 可观测性当初就是为它做的那个场景。

**证据链（三跳，全部为出厂默认 ON）**：

1. `src/server/gateway.rs:8007-8012` —— transient 类失败用
   `upstream_common_mode_transient_threshold`（默认 **4**，`src/state/types.rs:242`）做阈值；
2. `src/server/gateway.rs:8060` —— 连续失败数达阈值即 `common_mode_tripped = true`（闩锁，不复位）；
   同一 host 上 3 个候选第一轮全 502、加上 last-resort 探测的第 4 次 502，正好凑满 4；
3. `src/server/gateway.rs:8376` —— `should_aggregate` 的第一个合取项就是 `!common_mode_tripped`，
   闩锁一旦置位 `should_aggregate` 必为 `false`；
4. `src/server/gateway.rs:8380` 附近 —— 于是 `error = last_route_error`，即**上游原始错误直通**。

而 `common_mode_transient_pool_error`（`src/server/gateway.rs:1000-1040`，本身带
`common_mode` / `failed_route_count` / `distinct_hosts` / `streak` / `threshold` / `retried`
这些好字段）**也不会被构造**：`src/server/gateway.rs:8075` 显示**第一次**跳闸会把预算花在一轮
500ms 的 replay 重放上（`transient_pool_replay_done`），只有**第二次**跳闸才走
`last_error = Some(common_mode_transient_pool_error(...))` 然后 `break`。

**净效果**：客户端两头空 —— 既拿不到 T0 的 `attempt_count` / `routing_rounds` /
`give_up_reason` / `last_resort_probe_attempted` / `remaining_candidates`，也拿不到 common-mode
自己的字段，`error.details` 只剩 `{"request_id","scope":"upstream","upstream_status":502}`。
运维侧日志（`src/server/gateway.rs:8509`）字段仍然齐全，所以**只有客户端瞎，运维不瞎** ——
这也是它一直没被发现的原因。

**已钉住**：`tests/gateway/chat/rate_limits.rs::t21_probe_with_common_mode_on_loses_the_terminal_details_p4_gap`
把当前（错误的）行为按现状断言下来，并在注释里写明 P4 落地后要改成什么断言。**该测试不允许删除，
只允许改写。**

**修复方向（二选一，需实施者判断）**：

- **方案 A（推荐）**：把 `should_aggregate` 的 `!common_mode_tripped` 去掉，改为聚合 T0 details
  之后再把 common-mode 字段 **merge** 进同一个 `details`。理由：两组字段互补而不互斥，闩锁本来
  只该影响*是否继续重试*，不该影响*错误报告的丰富度*。
- **方案 B**：保留短路，但在 `common_mode_tripped` 分支里也构造一个带 details 的错误
  （复用 `terminal_route_failure_error` 再补 common-mode 字段）。

两个方案都必须保持：终态 HTTP 状态码与 `error.code` 不变（避免破坏既有客户端契约），只加 details。

### P5 出厂默认值变更的测试爆炸半径（**本轮已修完，但必须读懂，否则会再犯**）

P0.1 把 `upstream_transient_route_cooldown_base_seconds` 由 10 改 5、
`..._max_step` 由 3 改 2 之后，**6 个既有测试当场变红**，且全部在串行 `--exact` 下稳定复现
（即真回归，不是并发抖动）。根因分两类：

**类别一：测试把旧默认曲线硬编码进了断言窗口（4 个）**

| 测试 | 位置 | 症状 |
|------|------|------|
| `default_route_exhaustion_budget_waits_out_a_transient_cooldown` | `tests/gateway/chat/rate_limits.rs` | 断言等待 ≥7s，实测 4.65s（新曲线 base 5 抖动到 4–6s） |
| `budget_aligned_last_wait_refused_when_recovery_exceeds_budget` | 同上 | 注释写「8–12s 冷却远超 5s 预算」，新曲线 4–6s **有时装得进** 5s 预算 ⇒ 改去等待 ⇒ 撞 1s 超时 `Elapsed(())` |
| `upstream_5xx_with_nested_rate_limit_code_remains_transient` | `tests/gateway/chat/streaming.rs` | `Retry-After` 期望 `"9"`，实测 `"5"` |
| （同名重复用例） | `tests/gateway/responses/upstream_feedback.rs` | 同上 |

后两个尤其值得记住：base 改成 5 之后，本地曲线算出来的 5s **和
`upstream_retry_after_cooldown_cap_seconds`（5s）数值撞车**，该测试原本要区分的
「本地曲线 / 被裁剪的上游提示 / 上游原始 30」三者塌缩成两者，**测试失去鉴别力**——
即使断言改成 `"5"` 也是一个更弱的测试。所以修法是在测试里**显式写死 `base = 10`**，
而不是把期望值改小。

**类别二：`wait_budget` 这个终态在合规默认值下已经不可达（1 个）**

`route_retry_wait_budget_and_round_limit_are_bounded` 期望 `give_up_reason == "wait_budget"`，
实测变成 `"alignment_exhausted"`。这不是 bug，是 **T1.1 不变量的直接推论**：不变量
`ceiling * 1000 < upstream_route_exhaustion_retry_max_wait_ms` 恰好保证了「任何单次冷却都装得进
预算」，于是单次冷却撑爆预算的 `wait_budget` 路径在**合规配置**下不可达。

**这条测试原先之所以是绿的，正是因为它继承了那份违反 T1.1 的旧默认值。** 换句话说：
P0 修掉的那个缺陷，此前一直被一个测试当作前提在用。修法是在测试里显式写出这份
（故意违反不变量的）运维误配曲线 base=10 / max_step=3，并在注释里说明
「`wait_budget` 只在误配曲线上可达」。

**类别三：`upstream_transient_route_cooldown_max_step` 参与了断言的取值（1 个）**

`route_retry_last_resort_probe_interval_blocks_second_request_then_reprobes` 断言
`consecutive_failures == 3`，实测 2 —— 默认 max_step 由 3 降到 2 之后 step 直接饱和，
第三次探测再也推不到 3。修法同样是在该测试里显式写 `max_step: 3`。

**留给后续的硬约束（写进本节是为了让下一个人不必再踩一遍）**：

> 测试里凡是断言了**具体时长、具体 `Retry-After` 数值、或具体 `consecutive_failures`** 的用例，
> 必须在自己的 `AppConfig` 里**显式写出所依赖的冷却曲线参数**，不得继承
> `AppConfig::default()`。出厂默认值属于「随时可能因为不变量而被调整」的量，
> 让断言去继承它，等于把两件无关的事情耦合起来。

---

## 4. 测试要求

### 4.1 单元/集成清单

| # | 位置 | 断言 |
|---|------|------|
| 1 | `tests/runtime_settings.rs` | 出厂默认配置通过 `validate_and_normalize`（P0.3-1） |
| 2 | `tests/runtime_settings.rs` | 旧文档被自愈而非整份丢弃，无关字段保留（P0.3-2） |
| 3 | `src/state/types.rs` | 编译期 `const _` 断言（P0.1）——改坏默认值直接编译失败 |
| 4 | `tests/gateway/shared_host_failure_domain.rs` | 同 host ⇒ EdgeProxy 曲线 + step 不升级 + `cooldown_source` |
| 5 | 同上 | 不同 host 对照组 ⇒ 正常曲线 + step 升级 |
| 6 | `tests/gateway/last_resort_probe_after_attempts.rs` | 全 502 ⇒ 恰好一次探测，`last_resort_probe_attempted=true` |
| 7 | 同上 | 主开关关闭 ⇒ 不探测 |
| 8 | `tests/gateway/route_exhaustion_budget_invariant.rs` | 出厂默认值端到端：`routing_round>=2`、`give_up_reason!="wait_budget"`、`distinct_upstream_hosts==1`（P1.3） |
| 9 | `tests/gateway/dialect_retry.rs` | 中文 400 剥离重试成功，且**路由未冷却**（P1.4） |
| 10 | `tests/redis_runtime.rs` | Lua 参数串接（不依赖 Redis 实例） |

### 4.2 验证命令（**禁止用 `&&` 串联**）

上一轮出现过一次假 ✅，原因就是实施方把 `fmt && clippy && test` 用 `&&` 串起来，
前一步非零退出后面几步根本没跑。本轮必须逐步记录独立退出码：

```bash
rtk cargo fmt --check ; echo "fmt rc=$?"
rtk cargo clippy --all-targets --all-features -- -D warnings ; echo "clippy rc=$?"
rtk cargo test --lib ; echo "lib rc=$?"
rtk cargo test ; echo "all rc=$?"
```

基线：**1777 passed / 0 failed / 88 ignored（62 suites）**。本轮新增测试后 passed 应严格上升，failed 必须为 0。

---

## 5. 风险与回滚

| 风险 | 影响 | 缓解 |
|------|------|------|
| 冷却曲线由 `10/20/40` 变 `5/10` | 公网侧对持续故障路由的重探频率翻倍 | 有界（最长 10s 一次）；且"配置过不了自己的校验"严格更糟。运维可单独调高 base，只要满足不变量 |
| P0.2 自愈掩盖错配 | 运维看不到自己配错了 | 自愈路径打 `auto_corrected = true` 的 **error** 级日志并写明修正值；Admin 侧仍然硬拒（后果三保持不变，这是正确行为） |
| 新增集成测试跑在全默认 ON | 与既有关开关的测试断言冲突 | 新增独立文件，不改既有测试的配置；既有回归保护保持原样 |
| Redis 套件本地无实例 | 仍然未执行 | 回填表强制写明"未执行"，不得留空 |

回滚：P0.1/P0.2 是独立小改动，`git revert` 即可；P1/P2 纯新增测试，回滚无行为影响。

---

## 6. 任务回填表（实施后填）

| 任务 | 说明 | commit | 状态 |
|------|------|--------|------|
| P0.1 | 出厂默认值满足 T1.1 + 编译期断言 | 待填 | ✅ 已实施并验证 |
| P0.2 | 持久化设置自愈而非整份丢弃 | 待填 | ✅ 已实施并验证 |
| P0.3 | 默认值合规 / 自愈 回归测试 | 待填 | ✅ 已实施并验证（`tests/runtime_settings.rs` 37 passed） |
| P1.1 | 同 host 失败域覆盖 + `cooldown_source` 断言 | 待填 | ✅ 已实施并验证（**落在 `tests/route_health.rs`**，见下方偏差说明） |
| P1.2 | last-resort 探测（attempts 用尽后）覆盖 | 待填 | ✅ 已实施并验证（**落在 `tests/gateway/chat/rate_limits.rs`**，见下方偏差说明） |
| P1.3 | 出厂默认值端到端回归 | | ⬜ 未开始 |
| P1.4 | `dialect_retry.rs` 中文 400 + 路由未冷却 | | ⬜ 未开始 |
| P2 | Redis 参数串接测试 + 套件执行记录 | | ⬜ 未开始 |
| P3 | 清理 `GATEWAY_RETRY_TRACE` 脚手架 | 待填 | ✅ 已实施并验证（全仓 0 处） |
| P4 | common-mode 闩锁丢弃 T0 details | | ⬜ 未开始（缺陷已钉在测试里） |
| P5 | 默认值变更导致的 6 个既有测试回归 | 待填 | ✅ 已修完并验证 |

### 6.1 已完成部分的验证结果（截至 2026-08-26）

```
rtk proxy cargo fmt --check                                        rc=0
rtk proxy cargo clippy --all-targets --all-features -- -D warnings rc=0（0 条 warning/error）
rtk proxy cargo test --lib                                         rc=0   244 passed
rtk proxy cargo test                                               rc=0   1792 passed / 0 failed / 88 ignored / 62 suites
```

基线为 **1777 passed / 0 failed / 88 ignored / 62 suites**，净增 **+15**：
`tests/route_health.rs` +8（52→60）、`tests/gateway/chat/rate_limits.rs` +4、
`tests/runtime_settings.rs` +3。

### 6.2 与原方案的偏差（**必读，否则会重复造文件**）

| 原方案说的 | 实际落点 | 原因 |
|------------|----------|------|
| 新建 `tests/gateway/shared_host_failure_domain.rs` | `tests/route_health.rs`（+8 用例，含 4 个 T1.4 + 4 个 `cooldown_source`） | T1.4 的展平逻辑在注册表层（`src/state/route_health.rs:1372`），在注册表层测试是直接观察，走 HTTP 端到端反而要绕开一堆无关开关 |
| 新建 `tests/gateway/last_resort_probe_after_attempts.rs` | `tests/gateway/chat/rate_limits.rs`（+4 用例，`t21_*` 前缀） | 需要复用该文件里私有的 `spawn_retry_after_upstream` / `route_retry_downstream_config` / `route_retry_request` 等 helper；`tests/gateway.rs` 是 `#[path]` 聚合器，helper 是 `pub(crate)` 且仅对该 binary 可见 |

### 6.3 一个必须知道的 tracing 陷阱（P1.1 实施时踩到，已解决）

`tests/route_health.rs` 里断言日志字段的 4 个 `cooldown_source` 用例，最初用
`tracing::subscriber::with_default`（thread-local）捕获，**在过滤运行下能过、全量运行下抓不到东西**。

根因：tracing 对每个 callsite 的 interest 做**全局**缓存。该 binary 里另外 ~52 个用例只要有
任何一个在「没装 subscriber」的状态下命中过那个 callsite，`Interest::never()` 就被缓存下来，
**进程级**关掉该 callsite。`set_global_default` 会重建 interest 缓存，`with_default` **不会**。

因此这 4 个用例改用 `OnceLock` 守卫的**全局** `set_global_default`
（`tests/route_health.rs` 的 `fn cooldown_log_buffer()`），并靠每个用例独有的
`runtime_model_slug` 过滤各自的日志行。已在 3 次并行 + 1 次 `--test-threads=1` 下验证稳定。

**注意**：`set_global_default` 每个 test binary 只能调用一次。`tests/gateway.rs` 那个 binary 的
这一次已经被 `tests/gateway/responses/upstream_feedback.rs:1416-1425` 占用了 —— 谁要在
`tests/gateway/**` 下断言日志，必须复用它，不能自己再装一个。
