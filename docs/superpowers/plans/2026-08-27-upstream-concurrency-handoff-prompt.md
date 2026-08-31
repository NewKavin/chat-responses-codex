# 交接提示词 —— 上游并发槽位记账可信化 + 批量改字段

> 直接把下面整块内容贴给负责实现的模型。

---

你在 `/home/kavin/projects/chat2Responses`（Rust 网关，OpenAI Chat ↔ Responses 协议转换 + 多上游路由）上工作。

## 任务

按 `docs/superpowers/plans/2026-08-27-upstream-concurrency-lease-trustworthy-accounting.md` 实现 **C1 → C7**，顺序即优先级。先完整读那份文档，它已经把根因定位到 `file:line`，你不需要重新做排查。

## 背景（必须理解，否则会改错方向）

内网部署返回 `429 upstream_routes_exhausted / upstream concurrency limit saturated ... retried for 32.1s across 6 routing rounds`，但**上游没限流、请求根本没打到上游**。原因是网关自己的 pre-dispatch 本地租约闸门（`src/state.rs:3681-3689`）：当 `active_leases[(upstream_id, key_fingerprint)].len() >= max_concurrency`（默认 4）时直接拒绝，返回**硬编码 1s** 的 Retry-After，一次上游请求都不发。

而租约的释放路径全是软的——挂在 `runtime.spawn` 上且 JoinHandle 被丢弃、失败后永久卡在 `RELEASING` 无法重试、同步兜底用 `try_lock` 静默失败——所以真正的回收机制退化成 TTL，而 TTL 默认 **3600s**。**4 个泄漏租约把一个 key 钉死一小时。**

**关键约束：用户的上游账号 key 本身只允许 4 并发。所以"把 `max_concurrency` 调大"不是可选项。** 方案的核心是让槽位记账本身可信（C1/C2），并在 4 并发硬上限下把请求**排队服务掉**而不是拒绝（C3）。

两个容易踩的陷阱，文档里有详述，这里再强调：

1. **`C2.1` 必须先于 `C2.2` 合并。** 现在只有流式请求按 chunk 续约（`src/server/gateway/stream.rs:805,809,1607,1611`），非流式**完全不续约**。先把 TTL 从 3600 调小、后补心跳，会让长非流式请求中途丢租约 → 对只允许 4 并发的上游**真正超发**。
2. **同样的闸门有 3 处，不是 1 处。** `active_leases.len() >= max_concurrency` 判断 + 硬编码 `retry_after = 1` 出现在 `src/state.rs:3681`（普通请求）、`:3767` 和 `:3797`（都在 `try_reserve_upstream_account_hedge` 里）。只修第一处的话 hedge 路径照旧咬住槽位、照旧返回假的 1s。另外 hedge 吃的是**同一份** `max_concurrency` 预算——4 并发的账号上开 hedge 等于自己抢自己的槽位。
3. **多模型聚合到一个下游 key 时，C3 必须和 C7 一起上。** 下游并发闸门不认模型（`try_reserve_downstream_concurrency` 没有 model 参数，`src/state.rs:4640-4643`；计数只按 `downstream.id`，`:4669-4671`），而下游租约在 `src/server/gateway.rs:5233` 之前就拿到、选路循环在 `:6527` 才开始——**排队期间一直占着下游槽位**。只上 C3 的话，小容量模型（现场：glm5.2，容量 4）的排队请求会占满下游槽位，把大容量模型（deepseek，容量 28）饿死。这是队头阻塞，C3 单独上线会把它放大。
4. **报文里的「6 routing rounds」不是配置被人改过。** `ConcurrencySaturated` 走独立预算 `DEFAULT_UPSTREAM_CONCURRENCY_RECOVERY_MAX_ROUNDS = 32` / `MAX_WAIT_MS = 30_000`（`src/state/types.rs:261-262`，分支在 `src/server/gateway/route_retry.rs:305-315`），与 `upstream_route_exhaustion_retry_max_rounds`（3）不可比。别去"修"这个轮数。

## 规矩（项目硬性要求）

- **所有命令加 `rtk` 前缀**，包括 `&&` 串起来的每一段：`rtk git add <file> && rtk git commit -m "..."`，不是 `git add ... && rtk git commit`。
- **`rtk cargo test` 会吞掉失败**（曾在失败的运行上返回 rc=0）。跑测试必须：
  ```bash
  rtk proxy cargo test 2>&1 | tail -40
  echo "TRUE_RC=${PIPESTATUS[0]}"
  ```
- **验证步骤不要用 `&&` 串联**。曾因 `fmt && clippy && test` 短路误报过全绿。fmt / clippy / test 各跑一次，各自独立记录退出码。
- 首次全量测试要**核对套件数**。曾出现"只跑了 29 个套件"被当成全绿，因为运行在某个失败处提前中止。
- **不要 `git add .`，不要 `git commit -a`**。只 stage 你自己改的文件——`docs/superpowers/plans/` 下有几份尚未纳入版本管理的方案文档，别把它们混进你的提交。
- **不要跑 `cargo fmt --all`**（会格式化到别人的 WIP）。用 `rtk proxy cargo fmt --check` 看，只格式化自己碰过的文件。

## 基线

交接时的绿色基线（2026-08-27 实测，rc=0）：`rtk proxy cargo test` = **62 个套件 / 1795 passed / 0 failed / 88 ignored**。另据记录 `--lib` = **244 passed**，live Redis 套件（`TEST_REDIS_URL=… --test redis_runtime -- --ignored`）= **85 passed**。你的提交后不得低于此数；新增测试应让 passed 上升。

工作树在交接时是干净的（只有 `docs/superpowers/plans/` 下几份尚未 `git add` 的方案文档）。开工前先 `rtk git status` 确认一次。

## 已知既有问题（不要当成你引入的回归去修）

`chat::rate_limits::cancelled_account_waiter_does_not_block_the_next_request` 在满并行负载下偶发失败，隔离跑 rc=0。这是既有竞争。C3.2 改完后确认它在隔离下仍通过即可，**不要为它改产品代码**。

## 交付要求

1. **分批提交**，每个 C 任务（或紧密相关的一组）一个提交，每个提交都能独立编译 + 测试通过。
2. **每个提交后回填** `docs/superpowers/plans/2026-08-27-upstream-concurrency-lease-trustworthy-accounting.md` §6 的任务回填表：commit hash + 结果，通过打 ✅。**不要提前打 ✅。**
3. 全部做完后填 §6.1 验证结果表，**逐步骤独立记录退出码**。Redis 那行：跑了就写通过数，没跑就写「未执行」，**不要留空**。
4. §6.2 现场验证表留给用户在内网填，你不要编数据。
5. 如实汇报：有测试失败就贴输出说失败；有任务没做就说明是哪个、为什么。**不要用"应该没问题"代替实际运行结果。**

## 额外功能（C6 / C7，不要漏）

`/api/admin/upstreams` 已有 `/batch`（批量创建）、`/batch-toggle`、`/batch-delete`，**唯独缺批量改字段**——而修这次事故就需要一次性给多个账号调 `max_concurrency` / TTL。新增 `POST /api/admin/upstreams/batch-update`，body `{ ids: [...], updates: {...} }`，**复用 `state.update_upstream_by_id`**（`admin_update_upstream` 已经是 partial-merge 语义，见 `src/server/admin.rs:1883-1892`），风格对齐 `admin_batch_toggle_upstreams`（`:2390`）/ `admin_batch_delete_upstreams`（`:2424`），逐 id 返回成功/失败。字段走**白名单**，白名单外直接 400 并列出被拒字段名——**不要静默忽略**，静默忽略会让运维以为改成功了。

**C7** 是下游并发按模型分组：`DownstreamConfig` 加 `per_model_max_concurrency`，闸门计数键从 `downstream.id` 变成 `(downstream.id, 组)`，原 `max_concurrency` 退为全局兜底，字段为空时行为与改动前逐字节一致。详见方案 §C7。

## 注意：工作树正在被并发修改

交接文档写作时，`src/main.rs`、`src/server/gateway.rs`、`src/state.rs`、`src/state/runtime_settings.rs`、`src/state/types.rs` 已有未提交改动，其中 C1.1 看起来已经在落（三处闸门已从 `state.active_leases.get(...)` 变成 `table.account_lease_count(&account)`，行号也随之漂移到 `:3700` / `:3804` / `:3845`）。**文档里的所有 `file:line` 以你实际看到的代码为准**，先 `rtk git status` + `rtk git diff` 搞清楚已经做到哪一步，别重复实现或覆盖别人的改动。

## 前序状态

`docs/superpowers/plans/2026-08-26-t11-default-invariant-and-coverage-gaps.md` 的 P0–P5 **已全部完成并提交**，没有遗留任务要你带。那份文档修的是 transient/502 共模的冷却曲线与退避预算，与本次的 `ConcurrencySaturated` 本地闸门路径走不同预算分支（`src/server/gateway/route_retry.rs:305-315`），改动互不冲突——但**读一下它的 §3-P4 和 §6.3**，里面记了 common-mode 闭锁的落点和 tracing callsite 缓存的坑，你改 `route_retry` / 终态 details 时会用上。
