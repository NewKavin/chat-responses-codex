# SSE 解码错误可诊断化 + 全量测试栈溢出回归

- 日期：2026-08-30
- 状态：已完成（任务回填见 §6，验证结果见 §6.1）
- 前序：C1–C7、E1–E7、F1–F4 已落地。`e8b6e864 fix(stream): avoid compressed upstream SSE bodies` 修的是本方案 §2.2 的**其中一个**来源，方向正确但覆盖不全，本方案在其之上补齐，**不推翻它**。

## 1. 现象来源

1. 现场出现 `stream_upstream_body_decode_error`，用户判断是新问题。
2. 核对 F1–F4 交付时发现：`rtk proxy cargo test` **rc=101**，`troubleshooting` 测试二进制栈溢出被 SIGABRT 打死，整轮中止在 **56 套件 / 1773 passed**（完整应为 62 / 1844）。回填表记的 ✅ 来自分散跑单个测试，全量跑是红的。

## 2. 根因

### 2.1 全量测试栈溢出（G0，阻断性）

```
thread 'compatibility_matrix_does_not_queue_probes_or_mutate_runtime_state' has overflowed its stack
fatal runtime error: stack overflow, aborting
```
`tests/troubleshooting.rs`

**已确认是栈增长，不是无限递归**——`RUST_MIN_STACK=16777216 rtk proxy cargo test` ⇒ **rc=0，62 套件 / 1844 passed / 0 failed / 99 ignored**。也就是说除这一处外，F1–F4 的产出是好的（1844 比改动前的 1841 涨了 3）。

成因：F 系列往运行态快照结构、错误 `details` map 里加了字段，把兼容性矩阵那条异步 future 的栈占用推过了测试线程默认的 2MB。debug 构建下 future 不做布局优化，这类"加几个字段就炸"很典型。

**危害不止于红灯**：`cargo test` 中途 abort 时，前面的套件都打印了 `test result: ok`，输出**看起来非常像跑完了**。这正是交付时被误判为全绿的原因——所以这次也再次印证了"必须核对套件数"这条纪律。

### 2.2 同一个错误码，两个完全不同的来源（G1/G2）

| 来源 | 位置 | 触发条件 | 现有诊断 |
| --- | --- | --- | --- |
| **传输层** | `src/server/gateway.rs:4817-4826` `classify_upstream_stream_error`，把 reqwest 的 `is_decode` / `"error decoding response body"` 映射过来 | 解压失败、body 被中间代理截断 | 有 `log_stream_body_read_diagnostic`（带 `usable_output_exposed` / `semantic_terminal_observed` / `physical_attempt_count`） |
| **SSE 解析** | `src/server/gateway/stream.rs:2280-2285` `upstream_sse_decode_error()` | 帧不是合法 UTF-8，或 `data:` 载荷不是合法 JSON | **无任何诊断** |

`e8b6e864` 给流式请求加了 `Accept-Encoding: identity`，只作用于**传输层**那一条。如果现场的错误来自第二条（上游发了非 JSON 的 `data:` 帧——聚合网关中途插入错误文本、截断的 JSON、非标准哨兵值），这次修复不起任何作用。

**而目前无法判断是哪一条**：两条路给出同一个 `error.code`，第二条连一行日志都没有。排查只能靠猜——本次分析就卡在这里。

### 2.3 一个坏帧掐断整条已经在正常输出的流（G3）

`stream.rs:1128-1178` 的容忍链：

1. 非 UTF-8 ⇒ 报错；
2. 无 `data:` 行 ⇒ 跳过/透传；
3. 空 `data:` 或 `: ping` 保活（`sse_payload_is_keepalive`，`:2317-2320`）⇒ 跳过；
4. `payload.trim() == "[DONE]"` ⇒ 正常结束；
5. **其余一律 `serde_json::from_str`，失败即 502 掐断整条流**。

第 5 步不看流的状态：**哪怕前面已经吐了一大段可用内容、`usable_output_delivered` 已经是 true，一个坏帧照样让整个请求失败。** 对客户端来说，一次本可以正常收尾的回答变成了 502。

而判断所需的状态**都是现成的**：`self.usable_output_delivered`（`:1914`）、`self.commit_tracker.semantic_output_observed()`（`:1070`、`:1869`）。

### 2.4 新增测试锁的是机制不是结果（G2 的测试要求）

`e8b6e864` 新增的 `streaming_upstream_request_disables_content_encoding_negotiation` 只断言请求头发出的是 `identity`。它**没有复现"压缩流被截断 ⇒ 解码错误"，也就没有证明这个错误消失了**。锁住机制是必要的，但不足以说明修好了。

### 2.5 残留：`route_cooldown_skipped_total` 在 Redis 上是 null（G4）

F1.4 按方案要求把不支持的字段从"硬编码 0"改成 `null`，这是对的。但 `route_cooldown_skipped_total` **正是用来确认 E1（容量类失败不冷却路由）在生产上真的生效**的那个计数——生产走 Redis，于是运维在界面上依然看不到它。目标没有达成，只是不再误导。

## 3. 开发任务

### G0 — 修掉栈溢出（**最高优先级，先于一切**）

在这一条修好之前，任何人跑 `cargo test` 拿到的都是一个中途中止的假绿。

- 定位 `compatibility_matrix_does_not_queue_probes_or_mutate_runtime_state` 这条路径上栈占用最大的 future；
- **首选治本**：把大结构装箱（`Box::pin` 或把大字段 `Box` 起来），把栈占用降回去。F 系列加的那些快照/details 字段是直接诱因，但根因是这条 future 本来就已经接近上限；
- **不要**只在 `.cargo/config.toml` 里调大测试栈了事——那是掩盖症状，下次再加字段还会炸，而且掩盖之后连"炸了"这个信号都没有了。若确实需要，可以在治本之外**额外**加，但必须在提交说明里写清这是兜底而非修复；
- 加一条护栏：CI 或 `cargo test` 的验证步骤里核对套件数，套件数不足即失败（防止再次把中途 abort 读成全绿）。

### G1 — 让解码失败可诊断

- `upstream_sse_decode_error()`（`stream.rs:2280-2285`）改为携带诊断上下文：
  - 载荷的**有界摘录**（建议 ≤ 256 字节）、原始长度、在流中的字节偏移、已收到的帧序号；
  - 失败原因区分 `invalid_utf8` / `invalid_json`；
  - 摘录**必须脱敏**，并沿用既有的 `upstream_error_body_excerpt_enabled` 开关（`src/state/types.rs:467-468`）——该开关的既有约定是**只在内网自有上游、运维同时掌握两端时才开**，本任务不得改变这个约定，也不得在开关关闭时把摘录写进给客户端的响应；
- 摘录进 `tracing` 日志（始终）与 `error.details`（仅在开关打开时）；
- 复用传输层已有的 `log_stream_body_read_diagnostic` 的字段口径（`usable_output_exposed`、`semantic_terminal_observed`、`physical_attempt_count`、`routing_round`），两条路的日志字段要对得上，便于同一套检索。

### G2 — 两个来源拆成两个错误码

- 传输层 ⇒ `stream_upstream_transport_decode_error`（`gateway.rs:4826`）；
- SSE 解析 ⇒ `stream_upstream_sse_parse_error`（`stream.rs:2284`）；
- HTTP 状态码**均保持 502 不变**（客户端契约）；
- 新增开关 `stream_decode_error_code_split_enabled`（默认 **on**），关掉回落到统一的 `stream_upstream_body_decode_error`；
- `src/server/gateway/troubleshooting.rs:4193` 等按旧码分类的地方要同步识别两个新码，不要漏；
- **同码不同因是 F2 修过的同一类毛病**，这里是它在流式路径上的另一处实例，处理口径保持一致。

### G3 — 已经输出过的流不因一个坏帧被掐断

- 在第 5 步 JSON 解析失败时判断流的状态：
  - **尚未产生任何可用输出** ⇒ 维持现状，返回错误（此时失败是干净的，客户端重试有意义）；
  - **已经产生可用输出**（`usable_output_delivered == true` 或 `semantic_output_observed()`）⇒ **跳过该帧并计数**，让流继续，正常收尾（`[DONE]` / EOF）；
- 跳过的帧数计入新指标（G4），并在流结束时打一条 warn，含跳过总数与首个坏帧的摘录引用；
- 若跳过数超过阈值（新增 `stream_max_skipped_bad_frames`，建议默认 8）⇒ 仍然按错误终止，避免把一条已经彻底跑飞的流硬撑成"成功"；
- **注意不要**把坏帧透传给下游——下游客户端同样解析不了。跳过就是丢弃。

### G4 — 观测补齐

- 新增每上游计数：`sse_bad_frame_skipped_total`、`sse_parse_error_total`、`transport_decode_error_total`；
- **`route_cooldown_skipped_total` 在 Redis 后端补成真实计数**（§2.5）。它是确认 E1 生效的唯一直接证据，生产走 Redis，`null` 等于运维看不到。实现方式与 F1.4 已经做成真值的 `capacity_reject_total` 一致（Redis counters hash）；
- 上述计数接入管理端上游页的运行态详情（`frontend/src/views/admin/Upstreams.vue` 的 `formatRuntimeStateDetail`）。

### G6 — 观测字段补齐到"不留白"

- **`hold_p50_ms` / `hold_p95_ms` 在 Redis 后端仍是 `None`**（F1.4 的取舍）。这两个数是判断"要不要调并发""槽位周转多快"的直接依据，生产走 Redis ⇒ 现在看不到。二选一，不许留白：
  - 用 Redis counters/采样实现真值；或
  - 前端明确渲染为「本后端不支持」，而不是靠 `v-if` 静默隐藏——**隐藏和"值为 0"一样会让运维得出错误结论**；
- 同一口径检查所有 `Option` 化的诊断字段，避免出现"后端返回 null → 前端不显示 → 运维以为没发生"的链路。

### G7 — 纠正已作废的运维建议

F1.1 之前，`upstream_stream_max_duration_seconds` 兼任 Redis 租约时长，因此曾给出过"调小它可以压缩泄漏租约的滞留时间"的现场缓解建议（见 `2026-08-29-redis-lease-parity-and-terminal-naming.md` §6 末尾）。

**F1.1 之后这条建议已经失效**——租约时长改由 `upstream_local_lease_ttl_seconds` 决定，该设置回归它本来的职责（单条流的最长存活，消费点 `src/server/gateway.rs:1795`）。

- 在那份方案里就地标注该建议作废，并指向新口径；
- 部署文档里写清两者的分工，避免后来者按旧建议去调一个不再相关的参数。

### G8 — 方案文档纳入版本管理

`docs/superpowers/plans/` 下目前有 7 份**未 `git add`** 的方案文档（OAuth 登录、自助建 key、准入语义调优、Redis 租约对齐、本方案等）。干净 checkout 看不到它们，交接会断链。

- 单独提一个 `docs(plans)` 提交把它们纳入；
- 不要和代码改动混在同一个提交里。

### G5 — 文档

- 部署文档补一节「SSE 解码失败怎么排查」：两个错误码分别代表什么、看哪条日志、`upstream_error_body_excerpt_enabled` 的适用边界（内网自有上游）与风险。

## 4. 测试要求

**基线**：本方案实施前的真实基线是 **62 套件 / 1844 passed / 0 failed / 99 ignored**，但**必须先修 G0 才能在不加 `RUST_MIN_STACK` 的情况下跑出这个数**。开工第一步就是复现 rc=101，修完再复现 rc=0。

**验证纪律**：

```bash
rtk proxy cargo test > /tmp/verify.log 2>&1
echo "TRUE_RC=$?"
grep -E "^test result:" /tmp/verify.log | awk '{p+=$4; f+=$6; n++} END {printf "套件=%d passed=%d failed=%d\n", n,p,f}'
```

**套件数必须等于 62**。少于 62 说明中途 abort，即使每行都是 `ok` 也是失败——这次就是这么被误判的。fmt / clippy / test 各跑一次，各自独立记录退出码，**不要用 `&&` 串联**。不要 `git add .`，不要 `cargo fmt --all`。

### 4.1 G0

- 不设 `RUST_MIN_STACK` 时 `cargo test` rc=0 且套件数 = 62；
- 单独跑 `--test troubleshooting` rc=0；
- 若采用装箱方案，补一条断言该 future 大小的编译期或单元测试，防止再次悄悄长回去。

### 4.2 G1

- UTF-8 失败与 JSON 失败分别产生对应的 `reason`；
- 摘录长度被截断到上限，且**开关关闭时不出现在客户端响应里**；
- 日志字段与传输层诊断口径一致。

### 4.3 G2

- 传输层失败 ⇒ `stream_upstream_transport_decode_error`；SSE 解析失败 ⇒ `stream_upstream_sse_parse_error`；两者 HTTP 均为 502；
- 开关关闭 ⇒ 两者都回落为 `stream_upstream_body_decode_error`（回滚路径可用）；
- 按旧码分类的既有逻辑（如 `troubleshooting.rs:4193`）对两个新码仍然生效。

### 4.4 G3（本方案的核心验收）

- **流已经吐出可用内容后遇到坏帧 ⇒ 流正常收尾、客户端拿到完整的前半段 + 正常终止事件，不是 502**；
- 首帧就是坏帧（尚无可用输出）⇒ 仍然返回错误；
- 坏帧数超过 `stream_max_skipped_bad_frames` ⇒ 按错误终止；
- 坏帧**不被透传**给下游；
- 跳过计数正确。

### 4.5 补 `e8b6e864` 缺的那条（§2.4）

- 构造一个**真的返回 gzip 且中途截断**的假上游，断言：改动前会产生传输层解码错误、改动后（`Accept-Encoding: identity`）不再出现。现有测试只断言了请求头，没有覆盖结果。

### 4.6 回归

- C1–C7、E1–E7、F1–F4 的既有测试全绿，特别是 `tests/gateway/capacity_failure_no_cooldown.rs`（E1 门槛）；
- Redis 套件实跑：`TEST_REDIS_URL=… cargo test --test redis_runtime -- --ignored`，**不接受「未执行」**；
- 前端 `npm run type-check`、`npm test` 各自独立记录退出码。

## 5. 风险与回滚

| 风险 | 说明 | 处置 |
| --- | --- | --- |
| **调大测试栈掩盖问题** | 下次加字段再炸，且失去信号 | G0 要求治本；调栈只能作为额外兜底并在提交说明写明 |
| **摘录泄漏上游内容** | 载荷可能含用户数据 | 沿用 `upstream_error_body_excerpt_enabled` 既有约定（仅内网自有上游），关闭时不进客户端响应；长度上限 + 脱敏 |
| **G3 把真正跑飞的流撑成"成功"** | 坏帧不断但流不终止 | `stream_max_skipped_bad_frames` 上限 + 结束时 warn + 计数可见 |
| **拆错误码破坏客户端匹配** | 有客户端在匹配旧码 | HTTP 502 不变；`stream_decode_error_code_split_enabled` 可关 |
| **G3 掩盖上游质量问题** | 坏帧被静默跳过 | 必须有计数与 warn；G4 把它做到管理端可见，"能跑"不等于"没问题" |

**回滚**：G0 是纯修复不加开关；G2 由 `stream_decode_error_code_split_enabled` 控制；G3 由 `stream_max_skipped_bad_frames = 0` 即可退回"遇坏帧即失败"；G1/G4/G5 是增量。

## 6. 任务回填表

> 逐行回填 commit hash 与结果，通过打 ✅，未做写明原因。**不要提前打 ✅。**

| 任务 | 内容 | commit | 结果 |
| --- | --- | --- | --- |
| G0.1 | 修栈溢出（治本：装箱/降低 future 栈占用） | faf25ca7 | ✅ |
| G0.2 | 套件数护栏（不足 62 即失败） | faf25ca7 | ✅ |
| G1.1 | 解码失败携带 reason + 有界脱敏摘录 + 偏移 | d9e556bf | ✅ |
| G1.2 | 日志字段与传输层诊断口径统一 | d9e556bf | ✅ |
| G2.1 | 拆成 transport / sse_parse 两个错误码 | 405a7506 | ✅ |
| G2.2 | `stream_decode_error_code_split_enabled` 开关 + 旧码分类兼容 | 405a7506 | ✅ |
| G3.1 | 已有可用输出时跳过坏帧、流正常收尾 | 1cde26a3 | ✅ |
| G3.2 | `stream_max_skipped_bad_frames` 上限 + warn + 不透传 | 1cde26a3 | ✅ |
| G4.1 | 三个 SSE 计数接入快照与管理端 | `984e5b3` | ✅ `record_upstream_stream_counter`（本地同步 / Redis HINCRBY）+ 快照三字段 + admin 透传 + `formatRuntimeStateDetail` 渲染；补上 invalid_utf8 计数缺口；gzip 测试实测 `sse_parse_error_total >= 1` |
| G4.2 | **Redis 侧补齐 `route_cooldown_skipped_total` 真值** | `984e5b3` | ✅ Lua 第 16 元素返回 counters hash 真值，`parse_upstream_snapshot` 解析 `Some(x)`，测试 `Some(0)` 前观察为真值 |
| G6.1 | `hold_p50/p95` 补真值或前端显式「本后端不支持」 | `984e5b3`, `1ff5e8b` | ✅ 选「显式渲染」路径：快照新增 `hold_supported`/`queue_depth_supported`（Redis 为 false），前端 tooltip 与列内渲染「本后端不支持」，Redis 集成测试断言两标志为 false |
| G6.2 | 全量核查 Option 化诊断字段，消除"null → 不显示 → 误判" | `1ff5e8b` | ✅ 核查结论：Option 字段只剩 hold_p50/p95 与 route_cooldown_skipped_total；后者两后端皆真值（仅短暂读失败为 null，前端显示「本后端不支持」）；其余诊断字段均非 Option。前端 spec 测试锁定渲染契约 |
| G7 | 标注已作废的 stream_max_duration 缓解建议 + 文档分工 | `ee44e26` | ✅ `2026-08-29-…` §6 就地删除线标注 + 指向新口径；DEPLOYMENT 能力差异表补「两个时长设置的分工」节 |
| G8 | 7 份方案文档纳入版本管理（独立 docs 提交） | `c1d1661` | ✅ 独立 `docs(plans)` 提交，不与代码混排 |
| G5 | 部署文档：SSE 解码失败排查指南 | `ee44e26` | ✅ DEPLOYMENT 新增「SSE 解码失败怎么排查 (G5)」：两错误码来源/日志/排查步骤 + excerpt 边界 + 无 gzip 特性注记 |
| — | 补 `e8b6e864` 缺的 gzip 截断结果测试（§4.5） | `4b54f52` | ✅ `compressed_stream_truncation_stays_classified_and_never_reaches_decoder`：真 gzip 截断假上游，断言 sse_parse_error 而非 transport_decode_error、计数器一致；配套把 `truncated_stream_test_state` 参数化以便关闭网关内重试 |

### 6.1 验证结果回填

| 步骤 | 命令 | 退出码 | 结果 |
| --- | --- | --- | --- |
| fmt | `rtk proxy cargo fmt --check` | 0 | ✅ 通过 |
| clippy | `rtk proxy cargo clippy --all-targets` | 0 | ✅ 通过 |
| test | `rtk proxy cargo test`（无 `RUST_MIN_STACK`） | 0 | ✅ **62 套件 / 1851 passed / 0 failed / 99 ignored** |
| redis | `TEST_REDIS_URL=redis://127.0.0.1:6399 cargo test --test redis_runtime -- --ignored` | 0 | ✅ 实跑 96 passed / 0 failed |
| 前端类型 | `cd frontend && npm run type-check` | 0 | ✅ 通过 |
| 前端测试 | `cd frontend && npm test` | 0 | ✅ 37 文件 / 272 tests 通过 |

## 7. 运维待办（不是开发任务，交给用户在内网执行）

这些不需要改代码，但**不做的话前面的开发成果在生产上看不出效果**：

| 动作 | 为什么 | 怎么做 |
| --- | --- | --- |
| **把现存上游的 `max_concurrency` 从 4 调到 32** | E4 只改了**出厂默认值**，不影响已持久化的上游。不改的话本地闸门仍按 4 拦，E4 等于没生效 | 上游页逐个改，或 `POST /api/admin/upstreams/batch-update`，body `{"ids":[...],"updates":{"max_concurrency":32}}` |
| **确认 `upstream_local_lease_ttl_seconds` 的生效值** | F1.1 之后它才真正决定 Redis 租约时长；之前设了不生效 | 管理端设置页查看；建议 300 |
| **填写内网现场验证表** | 代码层已验证，生产效果只能在内网观测 | `2026-08-29-…-terminal-naming.md` §6 的排查命令；重试放大卡片现已按类别分色，`gateway_gate` 偏高才是网关的问题，`upstream_429` 偏高是上游的 |

**建议顺序**：先只调 `max_concurrency`，观察 10 分钟，再动别的。同时改多项会分不清是哪一项起的作用。
