# Redis 后端租约回收对齐 + 本地闸门终态命名统一

- 日期：2026-08-29
- 状态：待开发（交接给其他模型实现）
- 部署形态（已确认）：内网为 **PostgreSQL + Redis + 本项目**，即 `RuntimeCoordinationBackend::Redis`。**这一点决定了本方案的全部优先级**——C1/C2 修的是本地后端，生产走的是 Redis。
- 前序：C1–C7、E1–E7 已全部落地。本方案不推翻它们，是补它们在 Redis 路径上的缺口。

## 1. 现象来源

用户现场反馈（2026-08-29）：

> "上游观测的并发闸门怎么不自己回收，跑完了自己不回收，要等到超时才回收么？"
> "上游我去看了，实际并没有打满并发。"
> "`gateway_concurrency_saturated` 报错出现这个了，但是错误分类是 `upstream_routes_exhausted` 错误码 429，是不是这个也不对呀。"

两个独立问题：租约不回收（F1）、同一根因两个名字（F2）。

## 2. 根因

### 2.1 Redis 后端的租约时长根本不是 TTL 设置项（F1 主因）

```rust
lease_duration_ms: config
    .upstream_stream_max_duration_seconds
    .saturating_add(60)
    .saturating_mul(1_000),
```
`src/state/redis_runtime.rs:159-162`

**Redis 上游租约的时长 = `upstream_stream_max_duration_seconds + 60`，与 `upstream_local_lease_ttl_seconds` 无关。**

后果：运维在管理界面把 `upstream_local_lease_ttl_seconds` 调成 300 秒，对生产（Redis）**完全无效**。`upstream_stream_max_duration_seconds` 的取值通常是小时级（出厂默认 86400 = 24 小时），于是**一条泄漏的租约会占住槽位到一整天**。

对照 C1/C2 的落点，Redis 侧的缺口是系统性的：

| 机制 | 本地后端 | Redis 后端 |
| --- | --- | --- |
| 租约时长 | `upstream_local_lease_ttl_seconds`（默认 300s） | `upstream_stream_max_duration_seconds + 60`（可达 24h） |
| C1.2 `Drop` 中同步无条件释放 | ✅（`src/server/gateway.rs:3435-3445`） | ❌ 仍走 `runtime.spawn`（`:3448-3490`） |
| C1.3 释放失败可重试 | ✅ | ❌ `release_guard.complete()` 只在 `Ok` 时调用 |
| C2.1 心跳续约 | ✅ | ⚠️ `renew_upstream_request` 存在（`redis_runtime.rs:1389-1414`），但**必须实测确认心跳分发器真的走到了 Redis 分支** |
| C2.3 陈旧租约提前回收（`upstream_lease_stale_after_ms`） | ✅ | ❌ |
| `leaked_reclaimed_total` / `stale_reclaimed_total` / `hold_p50_ms` / `capacity_reject_total` / `route_cooldown_skipped_total` | 有真实值 | ❌ **硬编码 0/None**（`src/state/redis_runtime.rs:3315-3316`、`:3355-3356`） |

最后一行意味着：**生产环境的运维界面上，这些诊断字段永远是 0**——上一轮 E5 加的"免冷却放行"计数，在 Redis 部署上看不到任何变化。这不是没发生，是没上报。

### 2.2 Redis 上的回收语义

租约存为按到期时间打分的 ZSET：

- 正常释放 → `lease_release.lua` 的 `ZREM`，`in_flight` 立即下降；
- 泄漏（释放没跑成）→ 留在 ZSET 里，直到分值过期后被**惰性**剪除：`upstream_reserve.lua:15-16` 和 `upstream_snapshot.lua:8` 各自 `ZREMRANGEBYSCORE`。

所以"跑完了不回收"只有一种解释：**那条租约的释放没有执行**，而 Redis 上唯一的兜底是那个小时级的过期分值。用户的观察与代码一致。

> 注意惰性剪除的一个副作用：账号级 ZSET 只在**准入**时被剪，聚合级 ZSET 在**快照**时被剪。所以一条已过期的租约可能只存在于账号 ZSET 而不在聚合 ZSET 里。这不影响准入判断（准入会先剪再数），但会让人误读快照，排查时要知道。

### 2.3 同一根因两个终态名（F2）

本地并发闸门拒绝有两条出口：

| 出口 | code | category | HTTP | 触发条件 |
| --- | --- | --- | --- | --- |
| C4 快速失败 | `gateway_concurrency_saturated` | 同左 | 429 | 整轮**零物理尝试**、每个候选都被本地闸门拒 |
| 聚合耗尽 | `upstream_routes_exhausted` | 同左 | 429（`rate_limit_family` 时） | 其余全部情况 |

聚合那条的构造在 `src/server/gateway/errors.rs:300-326`；消息里的 "upstream concurrency limit saturated" 只是每路由的原因摘要（`errors.rs:24` 的 `FailureClass::ConcurrencySaturated => "upstream concurrency limit saturated"`），**不是错误码**。

于是只要这一轮里有任何一个候选真的发出过请求，本地闸门造成的失败就会对外报成 `upstream_routes_exhausted` —— **名字说"上游路由耗尽"，实际是网关自己拦的**。C4.2 的意图正是消除这种误导，但只覆盖了零物理尝试那一种情况。

同一个根因暴露成两个名字，直接后果有三个：客户端无法稳定匹配；运维按错误码检索会漏；E5.1 的重试放大指标虽然两类都计（`src/state.rs:3033-3041`），但**混在一个数里，答不了"是网关拒的还是上游拒的"**。

### 2.4 重试放大指标不分类（F3）

```rust
Some("upstream_routes_exhausted" | "gateway_concurrency_saturated" | "upstream_rate_limited") => {
    self.record_retry_terminal(&request.downstream_id, &request.model);
}
```
`src/state.rs:3033-3041`

三类合并成一个计数。`upstream_rate_limited` 是**上游真的回了 429**（E1 之后原样透传，是正确行为）；另外两类是网关侧。混在一起时，"6 次拒绝"这个数**无法区分**是上游在限流还是网关在拦——而这正是运维第一个要回答的问题。

## 3. 开发任务

### F1 — Redis 后端补齐 C1/C2（最高优先级）

**F1.1 租约时长改用 TTL 设置项**

- `src/state/redis_runtime.rs:159-162` 的 `lease_duration_ms` 改为由 `upstream_local_lease_ttl_seconds` 推导，与本地后端同口径；
- `upstream_stream_max_duration_seconds` 回归它本来的职责（单条流的最长存活），**不再兼任租约时长**；
- **强制顺序**：F1.2 的 Redis 心跳必须**先于**本项合并。先把时长从小时级降到 300 秒、后补心跳，长流式请求会中途丢租约 → 对上游真正超发。这与 C2.1 必须先于 C2.2 是同一个道理，不要重犯。

**F1.2 确认并补齐 Redis 心跳**

- 实测确认心跳分发器在 Redis 后端确实调用 `renew_upstream_request`（`src/state/redis_runtime.rs:1389-1414`）；
- 若没有接通，接上；若已接通，补一条**针对 Redis 后端**的续约测试（现有心跳测试是本地后端的）；
- 续约间隔沿用 `ttl/3`，与本地后端一致。

**F1.3 释放的可靠性对齐**

- Redis 释放仍需 async，无法照搬本地的同步 `Drop`；但要补 C1.3 的语义：**释放失败时把 `release_state` 回滚为 `ACTIVE`**，让后续 drop 或兜底扫描能重试，而不是永久停在 `RELEASING`（`src/server/gateway.rs:3471-3489`）；
- 增加一条 warn 日志计数：spawn 出的释放任务最终失败了多少次。现在这个数字完全不可见。

**F1.4 Redis 快照上报真实值**

- `src/state/redis_runtime.rs:3315-3316`、`:3355-3356` 的硬编码 0/None 改为真实统计：
  - `in_flight` 已经是真的（`ZCARD`），保留；
  - `stale_lease_count` / `oldest_lease_age_seconds` 可直接从 ZSET 分值算；
  - `capacity_reject_total` / `route_cooldown_skipped_total` / `hold_p50_ms` / `hold_p95_ms` 用 Redis 计数器/采样实现，或**明确标记为"本后端不支持"并在前端显示为"—"而不是 0**；
- **二选一必须做，不能继续report 0**：0 与"没发生"无法区分，会让运维得出相反结论。

**F1.5 Redis 侧陈旧租约回收（C2.3 对齐）**

- 在准入/快照的惰性剪除之外，增加按 `upstream_lease_stale_after_ms` 的提前回收；
- 回收计数单独上报，不混进正常释放。

### F2 — 本地闸门终态命名统一

- **判据**：终态的原因构成里，只要**存在** `FailureClass::ConcurrencySaturated` 且这些失败**来自本地闸门**（`physical_attempt_count == 0` 的那些候选），聚合终态的 `code`/`category` 就必须是 `gateway_concurrency_saturated`，而不是 `upstream_routes_exhausted`；
- 混合情况（本地闸门 + 真实上游失败同时存在）：保留 `upstream_routes_exhausted`，但 `details` 里必须给出 `local_gate_rejected_count` 与 `upstream_attempted_count`，让运维一眼看出成分；
- HTTP 状态码**保持 429 不变**（客户端契约）；
- 消息里继续保留每路由原因摘要，但**摘要不得与 code 冲突**——现在"code 说 routes_exhausted、消息说 concurrency saturated"就是冲突。

### F3 — 重试放大按类别拆分

- `record_retry_terminal`（`src/state.rs:3049`）增加类别维度，至少三类：`gateway_gate`（本地闸门）、`routes_exhausted`、`upstream_429`；
- `/api/admin/retry-amplification` 的 `points` 增加 `category` 字段，`total` 拆成分类小计；
- 前端「重试放大」卡片（`frontend/src/views/admin/Dashboard.vue`，本轮 `004d9d2b` 新增）按类别分色展示，让"是网关拒的还是上游拒的"**一眼可分**；
- 判读文案同步更新：`upstream_429` 偏高是上游的问题（E1 之后透传是正确行为），`gateway_gate` 偏高才是网关要处理的。

### F4 — 文档

- 部署文档补一节「Redis 后端与本地后端的能力差异」，逐项列出哪些设置在 Redis 上生效、哪些不生效。**这次的坑本质是运维改了一个在生产环境根本不读的设置项**，而文档没有任何地方说明这一点。

## 4. 测试要求

**基线**：实施前自己跑一次 `rtk proxy cargo test` 确认（最近记录 1841 passed，树在动，不要照抄）。

**验证纪律**：`rtk proxy cargo test 2>&1 | tail -40` + `echo "TRUE_RC=${PIPESTATUS[0]}"`；统计套件数要重定向到文件再统计；验证步骤不用 `&&` 串联；不要 `git add .`；不要 `cargo fmt --all`。

**Redis 测试必须实跑**：本方案的主体在 Redis 路径上，`--test redis_runtime -- --ignored` **不允许写「未执行」**。没有 Redis 就用 `docker run --rm -p 6379:6379 redis:7-alpine` 起一个。

### 4.1 F1

- **租约时长口径**：Redis 后端预留的租约到期时间 ≈ `now + upstream_local_lease_ttl_seconds`，**不再**是 `upstream_stream_max_duration_seconds + 60`；
- **长流式请求**：跑满 `> ttl` 的流式请求期间租约不被回收（证明 F1.2 心跳在 Redis 上真的生效）；
- **长非流式请求**：同上（这是 C2.1 在本地后端已覆盖、Redis 尚未覆盖的场景）；
- **释放失败可重试**：注入一次释放失败，确认 `release_state` 回滚且后续能再次释放；
- **快照真实性**：Redis 后端的 `stale_lease_count` / `oldest_lease_age_seconds` 反映真实 ZSET 状态；不支持的字段返回 `null` 而非 `0`；
- **陈旧回收**：泄漏租约在 `stale_after` 之后被回收，不必等到 TTL。

### 4.2 F2

- 单路由 + 本地闸门拒绝 + 本轮有物理尝试 ⇒ `code == category == "gateway_concurrency_saturated"`，**不是** `upstream_routes_exhausted`；
- 混合失败 ⇒ 仍为 `upstream_routes_exhausted`，且 `details.local_gate_rejected_count` 与 `details.upstream_attempted_count` 正确；
- 两种情况 HTTP 均为 429；
- 消息中的原因摘要与 `code` 不冲突。

### 4.3 F3

- 三类终态各自计数正确，互不串台；
- `/api/admin/retry-amplification` 返回带 `category` 的 `points`；
- 前端按类别分色（`npm run type-check`、`npm test` 各自独立记录退出码）。

### 4.4 回归

- C1–C7、E1–E7 的既有测试全绿，特别是 `tests/gateway/capacity_failure_no_cooldown.rs`（E1 门槛）与 `tests/gateway/upstream_concurrency_queue.rs`（C3）；
- 本地后端行为**逐字节不变**——本方案只动 Redis 路径与终态命名。

## 5. 风险与回滚

| 风险 | 说明 | 处置 |
| --- | --- | --- |
| **先降 TTL 后补心跳 ⇒ 真正超发** | 长请求中途丢租约，第 N+1 个请求被放行 | **F1.2 必须先于 F1.1 合并**，这是本方案唯一的强制顺序 |
| **改 `code` 破坏客户端匹配** | 有客户端在匹配 `upstream_routes_exhausted` | HTTP 状态码不变；新增开关 `upstream_local_gate_distinct_terminal_enabled`（默认 on），关掉回落旧码 |
| **Redis 快照改动影响前端** | 字段从 0 变成 null | 前端要能渲染 null 为「—」；只增不删 |
| **Redis 统计成本** | 采样/计数器增加 Redis 往返 | 用本地进程内聚合 + 定期上报，不要每请求多打一次 Redis |
| **混合失败判定写错** | 把真实上游失败误报成网关闸门 | 判据必须基于 `physical_attempt_count`，4.2 有专门测试 |

**回滚**：F1.1/F1.2 是正确性修复，不加开关（旧行为是缺陷）；F2 由 `upstream_local_gate_distinct_terminal_enabled` 控制；F3/F4 是纯增量。

## 6. 现场排查（内网，改代码之前先确认）

这次排查踩了一个坑：**本机 docker 环境与内网部署不是同一套**，本机的数据不能用来推断内网状态。以下命令请在**内网**执行。

```bash
# 1) 该上游账号的租约（替换成你的 key 前缀）
docker exec <redis容器> redis-cli --scan --pattern 'chat2responses:v1:upstream:*:account:*:leases'
docker exec <redis容器> redis-cli ZRANGE '<上面某个key>' 0 -1 WITHSCORES
docker exec <redis容器> redis-cli TIME
#   分值是到期时刻(ms)。分值 - 现在 ≈ 小时级 ⇒ 命中 §2.1；
#   条数远大于实际在跑的请求数 ⇒ 存在泄漏。

# 2) 实际生效的运行时设置
docker exec <pg容器> psql -U <user> -d <db> -t -A \
  -c "select document from runtime_settings;" | python3 -c "
import sys,json; d=json.load(sys.stdin)['settings']
for k in ('upstream_local_lease_ttl_seconds','upstream_stream_max_duration_seconds',
          'upstream_lease_stale_after_ms','upstream_capacity_failure_cooldown_enabled'):
    print(k,'=',d.get(k))"

# 3) 真实的上游并发上限
docker exec <pg容器> psql -U <user> -d <db> -t -A -F' | ' \
  -c "select name, max_concurrency from upstreams where active;"

# 4) 终态分类计数
docker logs <网关容器> --since 2h 2>&1 \
  | grep -oE "upstream_routes_exhausted|gateway_concurrency_saturated|upstream_rate_limited" \
  | sort | uniq -c
```

判读：

- 只有 `upstream_rate_limited` ⇒ 上游真在限流，网关行为正确（E1 透传），去查上游；
- 出现 `gateway_concurrency_saturated` 或 `upstream_routes_exhausted` ⇒ 网关侧在拦，结合 (1) 看是不是租约泄漏堆积；
- (2) 里 `upstream_local_lease_ttl_seconds` 是 300 但 (1) 的分值是小时级 ⇒ **直接坐实 §2.1**。

**临时缓解（不改代码）**：把 `upstream_stream_max_duration_seconds` 调小（例如 3600），泄漏租约的滞留时间会从小时级降到 1 小时。这是治标——它同时会限制单条流的最长存活，**如果你有超过该时长的长推理请求会被截断**，调之前先确认业务上可接受。

## 7. 任务回填表

> 逐行回填 commit hash 与结果，通过打 ✅，未做写明原因。**不要提前打 ✅。**

| 任务 | 内容 | commit | 结果 |
| --- | --- | --- | --- |
| F1.2 | 确认/接通 Redis 心跳续约 + Redis 侧测试（**先于 F1.1**） | `a2acb3f` | ✅ Redis 心跳调用链已确认接通；新增并实跑 `redis_upstream_lease_renewal_extends_lease_ttl`（1 passed） |
| F1.1 | Redis 租约时长改用 `upstream_local_lease_ttl_seconds` | `b5999d0` | ✅ Redis 初始配置与运行时热更新均改用本地 lease TTL；`redis_upstream_lease_uses_local_ttl_not_stream_duration` 实跑通过（1 passed） |
| F1.3 | 释放失败回滚 `release_state` + 失败计数日志 | `b25626c` | ✅ 状态回滚由 `LeaseReleaseGuard` 保证（已有 `failed_redis_releases_can_be_retried_by_a_clone` 覆盖）；新增 `redis_upstream_release_failures` 累计计数并写入 warn 日志；实跑通过 |
| F1.4 | Redis 快照上报真实值（或明确标记不支持，禁止继续 report 0） | `70ca854` | ✅ `stale_lease_count`/`oldest_lease_age_seconds` 从 ZSET 分值计算；`leaked_reclaimed_total`/`capacity_reject_total` 用 counters hash 真实计数；`route_cooldown_skipped_total` 改为 `Option`（Redis 上报 `null`，前端显示 —）；`hold_*` 保持 `None`。新增 2 个 Redis 集成测试实跑通过 |
| F1.5 | Redis 侧陈旧租约提前回收 | `1664024` | ✅ reserve/snapshot Lua 按 `stale_after` 提前回收并独立计入 `stale_reclaimed_total`；`redis_upstream_stale_lease_is_reclaimed_before_ttl` 实跑通过（1 passed） |
| F2.1 | 本地闸门终态统一为 `gateway_concurrency_saturated` | `1a20515` | ✅ 聚合路径按 `physical_attempt_count == 0 && local_gate_rejected_count > 0` 判定（且尊重 `upstream_local_gate_distinct_error_code_enabled` 回滚开关），code/category 统一为 `gateway_concurrency_saturated`（429 不变）；更新 `fast_fail_switch_off_keeps_gateway_concurrency_code` 并新增混合测试 |
| F2.2 | 混合失败的 `local_gate_rejected_count` / `upstream_attempted_count` | `1a20515` | ✅ 混合轮次保留 `upstream_routes_exhausted`，details 增加 `local_gate_rejected_count` 与 `upstream_attempted_count`；`mixed_local_gate_and_upstream_rejection_reports_composition` 实跑通过 |
| F3.1 | 重试放大按类别记录 + API 返回 `category` | `ac51be4` | ✅ 按 `(downstream_id, model, category)` 独立计数；API `points` 返回 `category`；单测验证三类互不串台 |
| F3.2 | 前端卡片按类别分色 + 判读文案 | `ac51be4` | ✅ Dashboard 按网关闸门、路由耗尽、上游 429 分色并显示分类小计；`npm run type-check` 与 `npm test` 通过 |
| F4 | 部署文档：Redis 与本地后端能力差异对照表 | `1d7bebe` | ✅ `DEPLOYMENT.md` 新增 Redis/本地能力差异表，并明确 `upstream_local_lease_ttl_seconds` 在 Redis 后端生效 |

### 7.1 验证结果回填

| 步骤 | 命令 | 退出码 | 结果 |
| --- | --- | --- | --- |
| fmt | `rtk proxy cargo fmt --check` | | |
| clippy | `rtk proxy cargo clippy --all-targets` | | |
| test | `rtk proxy cargo test` | | 套件数 / passed / failed / ignored |
| **redis** | `TEST_REDIS_URL=… cargo test --test redis_runtime -- --ignored` | | **必须实跑，不接受「未执行」** |
| 前端类型 | `cd frontend && npm run type-check` | | |
| 前端测试 | `cd frontend && npm test` | | |
