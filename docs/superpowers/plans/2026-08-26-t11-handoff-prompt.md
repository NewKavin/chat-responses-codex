# 交接提示词：T1.1 默认值不变量后续任务（P1.3 / P1.4 / P2 / P4）

> 配套方案文档：`docs/superpowers/plans/2026-08-26-t11-default-invariant-and-coverage-gaps.md`
> 本文件是**给实施模型的提示词**。下面 `====` 之间的内容可以整段复制给它。
>
> 交接时点：2026-08-26。此时仓库**是绿的**（`cargo test` rc=0，1792 passed / 0 failed /
> 88 ignored / 62 suites），P0 / P1.1 / P1.2 / P3 / P5 已完成，剩 **P1.3 / P1.4 / P2 / P4**。

---

## 一、交接状态速览

| 已完成（勿重做） | 落点 |
|------------------|------|
| P0.1 出厂默认值满足 T1.1 + **编译期**断言 | `src/state/types.rs`（base 10→5、max_step 3→2、`const _: () = {...assert!...}`） |
| P0.2 持久化设置自愈 | `src/state/runtime_settings.rs::repair_cooldown_ceiling_invariant`、`src/state.rs::validated_persisted_runtime_settings`、`src/main.rs:354-356` |
| P0.3 回归测试 | `tests/runtime_settings.rs`（+3）、`tests/docker.rs:355` |
| P1.1 同 host 失败域 + `cooldown_source` | `tests/route_health.rs`（+8，52→60） |
| P1.2 last-resort 探测 | `tests/gateway/chat/rate_limits.rs`（+4，`t21_*`） |
| P3 清脚手架 | `src/server/gateway.rs`(2) + `src/server/gateway/route_retry.rs`(4) → `tracing::debug!` |
| P5 默认值变更引发的 6 个既有测试回归 | 见方案文档 §3-P5，**修法是给测试显式写死曲线参数，不是改小期望值** |

| 待做 | 性质 |
|------|------|
| **P4** | 真实缺陷修复（唯一改生产码的任务） |
| **P1.3** | 新增测试：出厂默认值端到端 |
| **P1.4** | 新增测试：中文 400 剥离重试且路由不冷却 |
| **P2** | 新增测试：Redis Lua 参数串接 + 套件执行记录 |

---

## 二、给实施模型的提示词（整段复制）

```
====================================================================
你在 /home/kavin/projects/chat2Responses（Rust 网关，OpenAI Chat ↔ Responses
协议转换 + 多上游路由）上工作。当前分支 main，仓库是绿的，请先自己确认一次基线。

【必读背景：部署形态决定了这些任务为什么重要】
本网关部署在内网，所谓"多条不同线路"实际上**全部穿过同一个 new-api/one-api 聚合
网关**（同一台物理机、多个 key）。因此上游 502 是**共模故障**，路由多样性是假的
（`distinct_upstream_hosts == 1` 就是这个事实的指纹）。所有"换一条线路重试"的
逻辑在这个形态下都可能同时失败——这是下面每个任务的共同前提。

【强制约束，违反即返工】
1. 所有 shell 命令必须以 `rtk` 前缀执行，**包括 `&&` 链里的每一段**：
   ✅ `rtk git add <file> && rtk git commit -m "..."`
   ❌ `git add . && git commit -m "..."`
2. 要拿**真实退出码**时用 `rtk proxy cargo ...`。**`rtk cargo test` 会吞失败**
   （曾在有 1 个失败的运行里返回 rc=0 并吃掉 `test result:` 行）。正确姿势：
   `rtk proxy cargo test ... 2>&1 | tail -30` 然后 `echo "RC=${PIPESTATUS[0]}"`。
3. 验证步骤**禁止用 `&&` 串联**，必须逐条记录各自退出码。曾经有一次假 ✅ 就是
   `fmt && clippy && test` 短路造成的。
4. 绝不 `git add .`、绝不 `git commit -a`，只 stage 自己改的文件。
5. 不要跑 `cargo fmt --all`（共享工作树里可能有别人未格式化的 WIP），
   只 `rtk proxy cargo fmt -- <你改的文件>`。
6. 测试里凡断言**具体时长 / 具体 Retry-After 数值 / 具体 consecutive_failures**，
   必须在该测试自己的 `AppConfig` 里**显式写出依赖的冷却曲线参数**，不得继承
   `AppConfig::default()`。（上一轮 6 个测试变红就是因为继承了默认值，
   详见方案文档 §3-P5。）

【先读这两份文档，不要跳过】
- docs/superpowers/plans/2026-08-26-t11-default-invariant-and-coverage-gaps.md
  §3 是任务清单，§3-P4 是缺陷证据链，§3-P5 是上一轮的踩坑记录，§6.2 是落点偏差，
  §6.3 是一个 tracing 陷阱（会让你的日志断言测试静默失效）。
- docs/superpowers/plans/2026-08-25-route-exhaustion-cooldown-budget-invariant.md
  T1.1~T2.3 各机制的原始设计。

【任务，按此顺序做】

■ P4（唯一改生产代码的任务，先做，因为 P1.3 会观察到它）
修复：common-mode 闩锁会把终态错误的 `error.details` 全部丢掉。
证据链（自己去读这几行确认，不要只信我）：
  - src/server/gateway.rs:8007-8012  transient 阈值 = 4（src/state/types.rs:242）
  - src/server/gateway.rs:8060       达阈值即 common_mode_tripped = true（闩锁）
  - src/server/gateway.rs:8376       should_aggregate 第一个合取项是 !common_mode_tripped
  - src/server/gateway.rs:8075       第一次跳闸把预算花在 500ms replay，
                                     只有第二次才构造 common_mode_transient_pool_error
净效果：同一 host 上 3 候选全 502 + 一次探测 = 4 次，正好凑满阈值 ⇒ 客户端拿到的
`error.details` 只剩 {"request_id","scope":"upstream","upstream_status":502}，
T0 的 attempt_count / routing_rounds / give_up_reason / last_resort_probe_attempted /
remaining_candidates 和 common-mode 自己的字段**两头都没有**。运维日志
（src/server/gateway.rs:8509）字段是齐的，所以只有客户端瞎——这就是它一直没被发现的原因。

推荐方案 A：去掉 should_aggregate 里的 `!common_mode_tripped`，聚合出 T0 details 之后
把 common-mode 字段（common_mode / failed_route_count / distinct_hosts / streak /
threshold / retried，见 src/server/gateway.rs:955-1040）**merge 进同一个 details**。
理由：两组字段互补不互斥；闩锁本该只影响"是否继续重试"，不该影响"错误报告的丰富度"。
（方案 B：保留短路，但在 common_mode_tripped 分支里也构造带 details 的错误。二选一，
你判断，但要在文档里写明选了哪个、为什么。）

硬约束：终态 HTTP 状态码与 `error.code` **不得变化**（既有客户端契约），只加 details。

有一个测试已经把当前错误行为按现状钉住了：
`tests/gateway/chat/rate_limits.rs::t21_probe_with_common_mode_on_loses_the_terminal_details_p4_gap`
它的注释里写明了 P4 落地后要改成什么断言。**该测试只允许改写，不允许删除。**
另外 `t21_last_resort_probe_is_reported_and_happens_at_most_once` 目前是靠
`upstream_common_mode_same_host_transient_enabled: false` 隔离出来的；P4 修好之后
考虑能不能把这个隔离开关去掉——如果能去掉，说明 P4 修得彻底。

■ P1.3 出厂默认值端到端回归（覆盖原始线上事故现场）
用**未经任何覆盖的 `AppConfig::default()`**（这是整条测试的重点，一个开关都不许关），
6 条逻辑路由挂在**同一个 base_url** 上，全部 502 且带 `Retry-After: 28`，断言：
  - 请求至少完成一次轮间等待（`routing_rounds >= 2`）；
  - `give_up_reason != "wait_budget"`（T1.1 不变量的推论：合规配置下单次冷却必然
    装得进预算，所以 wait_budget 不该出现——见方案文档 §3-P5 类别二）；
  - `distinct_upstream_hosts == 1`（注意：这个字段是 **log-only**，不在 error.details 里，
    要断言就得走日志捕获，先读方案文档 §6.3 的 tracing 陷阱）；
  - 终态 503 的 `remaining_candidates` 是真实值。
现有套件全部跑在关掉开关的配置上，等于没覆盖事故现场——这条测试就是补这个。
参考 `tests/gateway/route_exhaustion_budget_invariant.rs` 的 `exhaustion_harness`
（3 上游 × 2 key 挂同一 base_url），但注意它**显式关掉了** T2.2/T1.4/T2.1/同路由重试/
hedge/探测，而 P1.3 要的恰好相反：全开。

■ P1.4 tests/gateway/dialect_retry.rs 补中文 400
  - 中文 400（GLM 数字码 `1210` + 字段名）⇒ 同路由剥离字段后重试**成功**；
  - **并断言该路由未被冷却**（健康注册表里仍可用、无冷却事件）。
后半条是重点：剥离重试成功的路由如果还被冷却，等于白重试一次。

■ P2 Redis 后端与参数串接
  - 新增一个**不需要 Redis** 的测试：断言 Lua 脚本文本/参数个数确实串接了
    T1.2/T1.3/T1.4 三个新参数（避免"改了内存后端忘了改 Redis 后端"这类漂移）；
  - 实际执行 `REDIS_URL=… rtk proxy cargo test --test redis_runtime -- --ignored`，
    把通过数记进方案文档 §6 回填表；
  - **本地无 Redis 时必须在回填表里明确写"未执行"，不得留空当成通过。**

【最终验证（逐条独立记录退出码，禁止 && 串联）】
  rtk proxy cargo fmt --check
  rtk proxy cargo clippy --all-targets --all-features -- -D warnings
  rtk proxy cargo test --lib
  rtk proxy cargo test
基线（你必须打平或超过，不许退化）：
  --lib      244 passed / 0 failed
  全量       1792 passed / 0 failed / 88 ignored / 62 suites
新增测试后 passed 应当**高于** 1792。若任何用例变红，先判断是"真回归"还是"并发抖动"：
用 `rtk proxy cargo test --test <suite> -- --test-threads=1 --exact <full::test::path>`
单独串行跑。已知 `chat::rate_limits::cancelled_account_waiter_does_not_block_the_next_request`
在满负载并行下偶发失败、单独跑必过，属于既有并发抖动，**不是你引入的**，不要去"修"它。

【收尾】
1. 回填方案文档 §6 任务回填表：commit hash + ✅；P2 若无 Redis 写"未执行"。
2. 提交时只 stage 自己改的文件，commit message 用 conventional commits
   （如 `fix(gateway): keep terminal details when the common-mode breaker latches`）。
3. 在回复里如实报告：哪些做完了、哪些没做完及原因、验证命令各自的退出码。
   如果某项做不动，把其余全部做完，然后明确说明留了什么、为什么——不要自行缩小范围。
====================================================================
```

---

## 三、交接方（我）本轮的两个判断失误，一并记录

1. **P0 缺陷本身是我上一版方案的遗漏**：T1.1 只写了不变量，却没有同步下调出厂默认值，
   导致 `AppConfig::default()` 通不过自己的 `validate_and_normalize()`。本轮已用**编译期**
   `const` 断言封死这个缺陷类别（`src/state/types.rs`），今后改默认值改到违反不变量会直接编译失败。
2. **改完默认值没有立刻跑网关套件**：base 10→5 之后 6 个既有测试变红，我是在实施完 P1.2
   之后才发现的。这 6 个的爆炸半径分析已写进方案文档 §3-P5，含"测试不得继承默认冷却曲线"
   这条硬约束。
