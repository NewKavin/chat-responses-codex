# 消除 `cancelled_account_waiter_does_not_block_the_next_request` 的时序竞争

- 日期：2026-08-30
- 状态：实施与验证完成，已归档（H4 护栏与反向验证结果见第 6 节）
- 范围：**仅测试侧**。本方案不改任何生产代码；如果实施过程中发现必须改生产代码才能修，那说明发现了真 bug，**停下来先报告**，不要顺手改。

## 1. 现象来源

`rtk proxy cargo test` 裸跑两次：

| 轮次 | rc | 套件数 | passed | failed |
| --- | --- | --- | --- | --- |
| 第 1 次 | **101** | 29 | 1282 | **1** |
| 第 2 次 | 0 | 62 | 1851 | 0 |

失败点：

```
thread 'chat::rate_limits::cancelled_account_waiter_does_not_block_the_next_request'
panicked at tests/gateway/chat/rate_limits.rs:930:5:
assertion `left == right` failed
  left: 2
 right: 1
```

即 `assert_eq!(harness.max_recovery_probes(), 1)` 实得 2。

已确认的性质：

- **隔离跑通过**（`--exact … --test-threads=1` ⇒ rc=0，445 filtered out）；
- **重跑全量通过**（62 套件 / 1851 passed）；
- 该测试文件最后一次改动是 `a47f2373`（C7），**与 G 系列无关**，不是新回归。

**危害不在于偶发红灯本身，而在于它长得和真回归一模一样。** 这次它制造了一个 `rc=101 / 29 套件` 的运行，与 G0 修复前那次栈溢出中止的表象几乎无法区分——每一次交付核对都要额外花一轮去排除它。

## 2. 根因：用墙钟和轮询代替屏障

### 2.1 出问题的测试（修复前历史快照，`tests/gateway/chat/rate_limits.rs:880-933`）

> 第 2 节的行号和代码片段记录**修复前**的失败现场（含 2.2 的 `:819-878`、`:1521-1530`、`:1616-1623` 等引用）；修复后对应位置为参考测试 `:822-882`、harness 屏障字段 `:1593-1600`、探测计数 `:1714-1716`。当前目标测试从
> `tests/gateway/chat/rate_limits.rs:884` 开始；当前行号以实际源码为准。

```rust
harness.set_accepted_delay(Duration::from_millis(250));   // ① 靠 250ms 让 request-1 停在飞行中
let first  = tokio::spawn(... "request-1" ...);
tokio::time::sleep(Duration::from_millis(1)).await;       // ② 靠 1ms 保证 1 先于 2
let second = tokio::spawn(... "request-2" ...);

tokio::time::timeout(Duration::from_secs(2), async {      // ③ 5ms 轮询一个"代理条件"
    loop {
        if harness.rejected_requests.load(SeqCst) == 0
            && harness.accepted_request_order().len() == 1 { break; }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}).await.expect("one recovery probe should start while the second request waits");

second.abort();                                            // ④ abort 是异步的
assert!(second.await.unwrap_err().is_cancelled());
...
assert_eq!(harness.max_recovery_probes(), 1);              // ⑤ 断言的却是探测并发峰值
```

四处时序假设，任何一处在满负载下失准就会翻车：

1. **① 250ms 的 accepted 延迟**：假设 request-1 会一直停在飞行中。CPU 争抢时这个窗口相对变短。
2. **② `sleep(1ms)` 排序**：这不是屏障，只是"大概率 1 先于 2"。
3. **③ 轮询的是代理条件**：`rejected == 0 && accepted.len() == 1` 只说明"有一个请求被接受了"，**并不保证只启动过一个探测**。5ms 的轮询粒度 + 调度延迟，足够让第二个请求的恢复会话在退出循环到 `abort()` 生效之间挤进一次探测。
4. **④ `abort()` 是异步的**：`is_cancelled()` 只证明 JoinHandle 被取消，**不证明网关侧的等待者已经摘除、也不证明它没再发出探测**。

而 ⑤ 断言的 `max_recovery_probes` 是 `active_recovery_probes` 的**并发峰值**（`fetch_add` 后取 max，见 `:1616-1623`），不是总次数。实得 2 意味着**两个探测曾同时在跑**——正是 ③④ 之间漏进来的那一个。

### 2.2 同一文件里已经有正确做法可抄

`one_key_shares_fifo_recovery_across_models`（`:819-878`）测的是几乎相同的场景，但用的是**屏障**：

```rust
harness.hold_rejection_responses_after_first();   // 卡住第二个拒绝响应
... 等待条件时用 tokio::task::yield_now()，不是 sleep ...
harness.release_held_rejection_response();        // 显式放行
```

harness 已经具备所需原语（`:1521-1530`）：`hold_rejection_responses`、`rejection_arrivals`、`all_rejections_arrived: Notify`、`release_held_rejection: Notify`。

**出问题的那个测试根本没用它们。** 所以这不是"难以避免的异步不确定性"，是同一文件内两种同步风格并存，其中一种是错的。

### 2.3 同类风险面

该文件共有 **6 处** `tokio::time::sleep`。每一处都要判断是"被测行为本身需要时间推进"还是"拿 sleep 当同步"，后者都是同一个雷。

## 3. 开发任务

### H1 — 补齐缺失的屏障原语

现有原语只能卡住**拒绝响应**，卡不住**恢复探测**。而本测试断言的恰恰是探测的并发度，所以需要一个直接的观测点：

- harness 增加探测生命周期信号：探测**开始**与**结束**各推进一个计数，并通过 `tokio::sync::Notify`（或 `watch` 通道）广播；
- 增加 `async fn wait_for_probe_started(n: usize)`：等待"累计已开始 n 次探测"，**替代对代理条件的轮询**；
- 保留现有的 `max_recovery_probes` 峰值语义**不变**（它是断言对象），只是额外暴露"累计开始次数"用于同步。

### H2 — 重写该测试，去掉全部墙钟假设

按 `one_key_shares_fifo_recovery_across_models` 的风格改写：

- **排序**：用屏障保证 request-1 先注册，删除 `sleep(1ms)`；
- **停住 request-1**：用 hold/release 屏障替代 `set_accepted_delay(250ms)`，让它停在飞行中直到测试显式放行；
- **等待探测**：改用 H1 的 `wait_for_probe_started(1)`，删除 5ms 轮询；
- **确认取消**：`abort()` 之后不要只断言 `is_cancelled()`，要等到**网关侧确实摘除了该等待者**的可观测证据（harness 侧计数或 `state` 快照），再继续；
- 轮询确有必要的地方一律用 `tokio::task::yield_now()`，**不用 `sleep`**（对照组就是这么做的）。

**不许放宽断言。** `max_recovery_probes() == 1` 测的是真实不变量——FIFO 恢复不得并发探测。把它改成 `<= 2` 或删掉，等于把这条保证丢了。**要修的是同步方式，不是期望值。**

### H3 — 审计同文件其余 4 处 sleep

- 逐处判断是"被测行为需要时间推进"（合法）还是"拿 sleep 当同步"（要改）；
- 属于后者的按 H2 同样处理；
- 判断结论写进提交说明，**即使结论是"这处合法、保留"也要写明理由**，避免下一个人重新纠结一遍。

H3 审计结论（修复前共 6 处，H2 删除其中 2 处后，当前源码剩余 4 处）：

| 位置 | 结论 |
| --- | --- |
| `:738` | **保留**。`concurrent_waiters_share_one_concurrency_probe` 的 mock upstream 用 150ms 保持响应 in-flight，制造真实的请求重叠窗口；这是被测并发行为需要的时间推进，不是任务排序同步。 |
| `:800` | **保留**。先推进配置的 100ms probe delay，再启动请求，验证 probe delay 已到期；这是被测延迟语义需要的时间推进。 |
| `:1738-1741` | **保留**。`accepted_delay_ms` 用于模拟慢 probe response headers，让 account recovery wait budget 真实取消探测；这是被测超时/取消行为需要的时间推进。 |
| `:3083` | **保留**。等待超过真实的 1 秒 last-resort probe interval，验证 interval 到期后允许新的 probe；这是被测 interval 语义需要的时间推进。 |
| 修复前目标测试的 `sleep(1ms)` | **删除**。它只试图排序 request-1/request-2，已由 rejection-arrival 屏障替代。 |
| 修复前目标测试的 `sleep(5ms)` | **删除**。它轮询代理条件，已由 rejection/probe lifecycle 屏障和 `yield_now()` 替代。 |

### H4 — 加一道防回归的护栏

- 该测试重复跑 N 次（建议 50）全绿才算通过：
  ```bash
  rtk proxy cargo test --test gateway -- --exact chat::rate_limits::cancelled_account_waiter_does_not_block_the_next_request --test-threads=1
  ```
  用循环跑，或用 `--test-threads` 提高并发制造压力；
- **必须在有负载的情况下验证**——这个 flake 只在满并行时出现，隔离跑 100 次全绿不能说明问题。建议在跑全量套件的同时循环跑它。

实际护栏执行采用 5 批串行批次；每批同时启动 1 个裸跑全量进程和 10 个
`--test gateway -- --exact ... --test-threads=1` 目标进程，目标进程和全量进程
保持重叠。先用 `rtk proxy cargo test --all --no-run` 预热构建，避免编译争用污染
压力结果。每个进程单独保存日志，并严格检查全量日志恰好有 62 条
`^test result:`，而不是只看最后一行。

## 4. 测试要求

**基线**：`rtk proxy cargo test` 裸跑（不加 `RUST_MIN_STACK`）应为 **62 套件 / 1851 passed / 0 failed / 99 ignored**，rc=0。

**验证纪律**：

```bash
rtk proxy cargo test > /tmp/verify.log 2>&1
echo "TRUE_RC=$?"
grep -E "^test result:" /tmp/verify.log | awk '{p+=$4; f+=$6; n++} END {printf "套件=%d passed=%d failed=%d\n", n,p,f}'
```

**套件数必须等于 62**；少于 62 说明中途 abort。fmt / clippy / test 各跑一次，各自独立记录退出码，不要用 `&&` 串联。不要 `git add .`，不要 `cargo fmt --all`。

### 4.1 决定性验收

- **全量套件连跑 5 次，5 次都是 62 套件 / 0 failed**。这是本方案唯一有意义的验收——跑一次绿不能证明 flake 消失；
- 目标测试单独重复 50 次全绿；
- 断言 `max_recovery_probes() == 1` **保持原样**（若提交里改了这个期望值，视为未完成）。

### 4.2 不得破坏的东西

- `one_key_shares_fifo_recovery_across_models` 等同文件其余测试全绿；
- **生产代码零改动**（`git diff --stat` 只应出现 `tests/`）。若确实需要改生产代码，先停下报告。

## 5. 风险与回滚

| 风险 | 说明 | 处置 |
| --- | --- | --- |
| **把 flake 改成"永久绿但没测到东西"** | 屏障加错位置，可能让被测竞态根本不发生 | 改完要能证明：故意破坏生产逻辑（临时注释掉等待者摘除）时该测试必须失败。**没做这一步就等于没验证** |
| **放宽断言当成修复** | 最省事也最有害 | H2 明确禁止；验收会检查期望值未变 |
| **新屏障自身引入死锁** | Notify 漏通知会让测试挂死 | 所有等待都要包 `tokio::time::timeout`，超时消息写清等的是什么 |
| **只在隔离下验证** | flake 只在满负载出现 | H4 要求带负载验证 |

**回滚**：纯测试改动，`git revert` 即可，不影响生产。

## 6. 任务回填表

> 逐行回填 commit hash 与结果，通过打 ✅，未做写明原因。**不要提前打 ✅。**

| 任务 | 内容 | commit | 结果 |
| --- | --- | --- | --- |
| H1 | harness 增加探测开始/结束信号 + `wait_for_probe_started` | `05b8df1c` | ✅ |
| H1' | 探测计数改为 Drop guard，取消路径同样记账（原实现在 `set_accepted_delay` 被预算取消时漏记）| `d4747109` | ✅ |
| H2 | 重写目标测试，去掉 4 处墙钟假设，断言不变 | `4074b302` | ✅ |
| H3 | 审计同文件其余 4 处 sleep（含保留理由）| `949b5c52` | ✅ |
| H4 | 重复跑护栏 + 带负载验证 | 仅验证，无代码改动 | ✅ 5×全量 + 50×目标，全绿 |
| — | 反向验证：破坏生产逻辑时该测试必须失败 | 临时改动，未提交 | ✅ 见 6.1 末行 |

### 6.1 验证结果回填

| 步骤 | 命令 | 退出码 | 结果 |
| --- | --- | --- | --- |
| fmt | `rtk proxy cargo fmt --check` | 0 | ✅ 无差异 |
| clippy | `rtk proxy cargo clippy --all-targets` | 0 | ✅ 零 warning / error |
| test ×5 | `rtk proxy cargo test`（连跑 5 次，每次与 10 个目标进程重叠）| 0 / 0 / 0 / 0 / 0 | ✅ 逐次均为 62 套件 / 1851 passed / 0 failed / 99 ignored（第 1~5 次完全一致，与基线相符）|
| 目标测试 ×50 | 5 批 × 10 进程，与全量裸跑并发重叠（4 核约 3.5 倍超订）| 全部 0 | ✅ 50 通过 / 0 失败 / 0 未完成，全部日志无 panic |
| 反向验证 | 注释掉 `AccountRecoverySession::drop` 里的 `cancel_account_waiter` 循环后跑目标测试 | 101 | ✅ 必然失败：`rate_limits.rs:967` panic `aborting request-2 must remove its account waiter before request-3 starts: Elapsed(())`；验证后已还原，`git diff src/` 为空 |
