# 会话级续写钉死（continuation pin）逃生方案 实施计划（待执行）

**日期：** 2026-08-21
**关联：** `2026-08-20-upstream-retry-after-cap.md`（T1–T9 已落地）、
`2026-08-08-intranet-codex-reliability-continuation-failover.md`（V2 契约失败转移，已落地）

**Goal:** 让一个已经踩到 `upstream_routes_exhausted` 的 codex 会话能够自愈——不再出现
「同一会话里发多少次『继续』都是同一个 503，只有退出 codex 开新会话才能恢复」。

**Architecture:** 续写请求当前被 `route_matches_profile_constraint`**硬过滤**到「契约相等」的少数路由
（常常只有 1 条）。本方案在主路由循环耗尽后增加一次 **续写逃生轮（continuation escape pass）**：
去掉契约约束、对已物化的历史做跨供应商净化、在全池重跑一轮，成功后把续写状态重钉到新路由。

**Tech Stack:** Rust / axum 0.8 / Vue3 + TypeScript（前端仅设置项）。

---

## 1. 症状与判据

**症状（生产实锤，2026-08-21 / 2026-08-22 补充）：**
1. 会话中某次请求返回 503 `upstream_routes_exhausted`；
2. 之后在**同一个 codex 会话**里反复发「继续」，每次都是同一个 `upstream_routes_exhausted`；
3. 看起来「完全不尝试其它上游账号」；
4. **退出 codex、开新会话就立刻正常**；
5. **（2026-08-22 补充）任务跑完、会话空闲一段时间后再下新指令，同样立刻报这个错。**

> **第 5 条的诊断价值很大**：空闲期间并发已经归零、瞬态冷却（内网配置 max 60s）也早就到期了，
> 却依然必现——**说明卡住的东西既不是负载也不是短冷却，而是「会话绑定 + 长期不可用的那条路由」**。
> 这把根因锁死在两条上：R1（续写 pin 无逃生口）与 R5（本地并发槽泄漏，永不归还）。


**判据（怎么确认就是这个根因）：**

| 判据 | 位置 | 说明 |
|------|------|------|
| 卡死的请求体里有 `previous_response_id` | 客户端/网关访问日志 | 新会话没有它，所以新会话好使 |
| `details.route_count` / `cooled_candidate_count` **恒等于 1**（或极小） | 终态错误 details | 池子明明有 N 条路由，候选却只剩 1 条 = 被续写契约过滤掉了 |
| `details.class_counts` 只有一类，且每次「继续」完全一样 | 终态错误 details | 一直在撞同一条路由 |
| 管理页「能力档案」里只有**部分 key** 有 Verified 档案 | Admin → 能力/探测 | 未探测的 key 无法承接续写（见 R2） |
| **空闲时某上游的 `in_flight` 不归零** | Admin 上游运行时快照（`admin.rs:622` 暴露 `in_flight`） | **并发槽泄漏实锤**（见 R5）。没有流量还占着 4/4，就是它 |

> **注意与另一个错误码区分**：如果契约**一条路由都匹配不上**，走的是
> `gateway.rs:5316` 的 `!required_route_available` 分支，返回
> `upstream_routes_temporarily_unavailable`(503) 或 `gateway_protocol_capability_unsupported`(400)。
> 本方案针对的是**匹配得上但全都不可用**的情形，终态码是 `upstream_routes_exhausted`。

---

## 2. 根因链（已核对代码）

### R1（主因）续写契约是硬过滤，且没有任何逃生口

1. 请求带 `previous_response_id` → `prepare_response_history_context_with_replay`
   （`src/server/gateway.rs:3762`）加载历史，得到 `_gateway_continuation` 状态；
2. `continuation_profile_key`（`gateway.rs:5114`）= 上次成功那条路由的
   `DialectProfileKey{upstream_id, key_fingerprint, runtime_model_slug, protocol}`；
   `route_profile_constraint_active = true`（`:5118`）；
3. `route_matches_profile_constraint`（`gateway.rs:5119-5203`）作为**硬条件**接进
   `route_is_candidate`（`:5428` 附近）——不满足契约的路由**根本不进候选集**；
4. 主路由循环 `'routing_rounds` 只在这个被削到 1–2 条的候选集上转；这些路由冷却/半开/并发满
   → 终态 `upstream_routes_exhausted`；
5. 失败不会写新的 response 历史 → 客户端下次「继续」仍带**同一个** `previous_response_id`
   → 同一个 pin → 同一个 503。**闭环，永不自愈。**

对比：普通的 routing affinity 是**软偏好**，有 `routing_affinity_escape_pressure_ratio`
逃生阈值（`gateway.rs:5764`）；续写 pin **没有**任何等价机制（代码注释自己写了
「continuation history pinning is stricter and applies even when multiple candidates are available」，
`gateway.rs:5696`）。

### R2 契约相等的失败转移在内网形态下常常够不着

`continuation_contract_for_route`（`capability_routing.rs:316`）在以下任一情况返回 `None`
（= 该路由不能承接续写）：

- `profile.state == DialectProfileState::Unknown` —— **该 key 从没被能力探测过**；
- `profile.probe_schema_version != DIALECT_PROBE_SCHEMA_VERSION` —— 探测数据过期；
- 路由的 resolved 能力不覆盖续写记录的 required capabilities。

用户形态是「多个 key、同一个 base_url」，`continuation_provider_group`（`:300`）按
`normalize_route_base_url + runtime_model_slug` 派生 → **同组**，本该能互相接管；
但只要其它 key 没有 Verified 档案，契约就派生不出来，失败转移池实际=1。

### R3 单候选 + 跨请求 step 升级 = 冷却被自己顶到天花板

`failure_step`（`route_health.rs`）在 `FAILURE_STREAK_RESET = 10min` 窗口内**跨请求**逐次 +1。
用户每隔几秒发一次「继续」，每次都撞同一条 pin 住的路由：
2s → 4s → 8s → … → `upstream_transient_route_cooldown_max_seconds`（内网配置 60s）并**长期驻留**。
（请求内抑制 A1 只在单个请求内生效，跨请求不适用。）

### R4 关键事实：续写其实**不依赖上游会话状态**

`prepare_response_history_context_with_replay` 默认 `replay_prior_history = true`
（`gateway.rs:3759`），它把历史 items 物化进 `input` 并**显式删除 `previous_response_id`**
（`gateway.rs:3821-3822`）：

```rust
object.insert("input".into(), Value::Array(effective_input_items.clone()));
object.remove("previous_response_id");
```

**即：发往上游的请求本来就是自包含的。** pin 存在的理由不是「上游存着会话」，而是
**保真度**——历史里可能夹带供应商绑定的产物（encrypted reasoning、thinking 签名、
特定 reasoning carrier 的 item 形状）。这意味着「换一条路由重放」在技术上完全可行，
代价只是这些产物需要被净化掉（模型重新推理一次），**远好于会话直接死掉**。

### R5（2026-08-22 新增）本地并发租约没有 TTL、没有回收器，泄漏即永久

这条单独就能解释「空闲之后依然报错」。

- 每个 `(upstream_id, key_fingerprint)` 账号的并发上限由 `upstream.max_concurrency`（默认 **4**）限制，
  本地后端的计数就是一个内存 `HashMap<AccountConcurrencyKey, HashSet<lease_id>>`
  （`src/state.rs:6320` `active_leases`）；
- 归还只有一条路径：`UpstreamRequestGuard` 的 `Drop` → `spawn_release()`
  （`src/server/gateway.rs:3050`）→ `release_upstream_request`（`state.rs:3835`）把 lease_id 从集合里删掉；
- **`UpstreamRequestLease` 自己没有 `Drop` 实现**（`state.rs:417-421`），
  **本地后端也没有任何 TTL / 后台回收器**（对比 Redis 后端：`upstream_reserve.lua` 的 lease 带
  `lease_duration_ms`，每次 reserve 都会 `ZREMRANGEBYSCORE` 清理过期项，天然自愈）；
- 因此只要有一次没走到 `Drop`——最典型的是
  `Handle::try_current()` 失败那条分支（`gateway.rs:3057-3066`，日志
  `"upstream request guard dropped outside Tokio runtime"`）——**那个槽位就永久占用，直到网关进程重启**；
- 4 个槽泄漏满 → 该 key 永远 `LocalConcurrency` → `ConcurrencySaturated`；
  叠加 R1 的 pin，该会话**永远**打不进去，空闲多久都没用；开新会话可能落到另一把 key 上，所以「新会话就好」。

**现场自证**：管理页看该上游的 `in_flight`——完全没有流量时它应该是 0。
不归零就是泄漏；此时重启网关会立刻「治好」所有卡死的会话（但这只是掩盖，不是修复）。

---

## 3. 立即缓解（不改代码）

1. **给所有 key 补能力探测**（最直接）：管理页对每个上游的**每一把 key** 跑一次能力探测，
   让它们都拿到 Verified 档案。这样 R2 消失，现有的契约失败转移就能真的把续写接管过去。
2. **每个上游 `max_concurrency` 4 → 16–64**：pin 会把整个会话压在**一把 key** 的 4 个并发槽上，
   codex 的并行子任务很容易打满（这也是 `2026-08-20` 方案 T10 的第 2 条）。
3. **`upstream_transient_route_cooldown_max_seconds` 60 → 20**：缩短 R3 的天花板，
   让被 pin 住的路由更快恢复（副作用：对真故障上游的退避变短）。
4. **应急口诀**：会话卡死时，退出 codex 重开新会话即可（不带 `previous_response_id`）。
   本方案落地后这一步就不需要了。

5. **先查 `in_flight`（R5）**：管理页看每个上游的 in_flight，空闲时应为 0。
   若不归零 → 并发槽已泄漏，**重启网关**可立刻恢复所有卡死会话（临时手段，P7 才是修复）。
   在 P7 落地前，若部署里启用了 Redis 运行时协调，可优先用 Redis 后端——它的 lease 带 TTL，自愈。

---

## 4. 目标与不变量

**目标：** 任何一个已经拿到过成功响应的会话，都不会因为「上次那条路由不可用」而永久卡死；
逃生最多让本次请求多花一轮路由时间。

**不变量：**
- 逃生**只在续写契约候选集耗尽后**发生，正常路径的路由/契约语义完全不变；
- 逃生优先级严格：① pin 的那条路由 → ② 契约相等的路由（现有 V2 失败转移）→ ③ 净化后的全池；
- 逃生每个请求最多一次；
- 逃生成功后必须把续写状态**重钉到新路由**（否则下次又走老 pin）；
- 400 族（请求形状被拒）**不触发**逃生——那不是路由问题；
- 429 族仍交还客户端（B3 不变量）。

---

## 5. 任务清单

### P1 逃生开关与设置项接线

- [ ] RED：`tests/runtime_settings.rs` / `tests/admin_runtime_settings.rs` 字段计数 +1；
      校验用例（默认 true、PUT 后 GET 回读）。
- [ ] GREEN：
  - `src/state/types.rs`：`AppConfig.upstream_continuation_pin_escape_enabled: bool`
    + `default_upstream_continuation_pin_escape_enabled() = true`；
  - `src/state/runtime_settings.rs`：字段（`#[serde(default)]`）+ 加入
    `IMMEDIATE_RUNTIME_SETTING_FIELDS` + `from_app_config` / `apply_to_app_config`；
  - `src/main.rs`：`env_bool("UPSTREAM_CONTINUATION_PIN_ESCAPE_ENABLED", true)`。
- [ ] 验证：`rtk cargo test --test runtime_settings --test admin_runtime_settings`。

### P2（核心）续写逃生轮

- [ ] RED：新增 `tests/gateway/responses/continuation_escape.rs`（注册到
      `tests/gateway/responses.rs`）：
  - **主用例**：两个上游（同 base_url、不同 key，均有 Verified 档案）。
    第一次请求成功并落 pin 到 up-A；把 up-A 打成冷却；带 `previous_response_id` 再请求
    → **必须成功**（走 up-B），而不是 `upstream_routes_exhausted`；
  - **重钉**：上一步成功后再发一次续写 → 直接命中 up-B（无需再逃生，
    断言 up-A 收到的请求数不增加）；
  - **净化**：历史里含 `reasoning` + `encrypted_content` 的 item，逃生后发往 up-B 的请求体里
    这些供应商绑定字段已被剥离，其余对话内容**逐条保留**（断言 items 数量与文本内容）；
  - **不越界**：pin 路由返回 400（`RequestRejected`）→ **不逃生**，直接把 400 交还客户端；
  - **开关关闭** → 现状行为（`upstream_routes_exhausted`）；
  - **逃生也失败** → 终态 details 含 `continuation_pin_escaped: true`，
    错误码仍为 `upstream_routes_exhausted`。
- [ ] GREEN（`src/server/gateway.rs`）：
  1. 把 `route_profile_constraint_active` / `route_matches_profile_constraint` 从「常量闭包」
     改成受一个 `continuation_constraint_relaxed: bool` 控制：relaxed 时该闭包恒返回 `true`
     （其余候选条件不变）。注意 `route_is_candidate` / `upstream_has_candidate_route` /
     `candidate_passes` 三处都要看到同一个开关。
  2. 在 `'routing_rounds` 结束、构造终态错误**之前**插入逃生判定：
     ```text
     if 开关开 && route_profile_constraint_active && !escaped_once
        && 终态是 Temporary/MixedRoutesExhausted 族（非 400/capability 族）
        && 本请求 physical_attempt_count 全部失败或为 0
     then
        净化 body 的历史 → continuation_constraint_relaxed = true
        → escaped_once = true → 重置 request_route_attempts（next_round 语义）
        → continue 'routing_rounds
     ```
  3. **净化**：新增 `fn sanitize_history_for_cross_provider_replay(body: &mut Value)`，
     复用既有 `strip_responses_chat_fallback_extensions` /
     `simplify_responses_input_for_chat_fallback`（`gateway.rs:3833` 一带）的思路，
     至少剥离：`reasoning` item 的 `encrypted_content`、网关签发的 thinking 签名
     （`thinking_signature::is_gateway_issued_thinking_signature`）、
     以及来源供应商的 item `id`。**必须保留**所有文本/工具调用内容。
  4. **重钉**：逃生成功后写回的续写状态必须是新路由的
     （`upstream.rs:1274/1605/1694` 一带已有「把选中路由存为新 preference」的逻辑，
     确认它在逃生路径上同样生效；不生效就补）。
  5. 日志：逃生时 `tracing::warn!(route_action = "continuation_pin_escape", pinned_route_id, ...)`。
- [ ] 验证：新用例 + `rtk cargo test --test gateway`（全量回归，重点
      `tests/gateway/responses/history.rs` / `reasoning.rs` / `tools.rs`）。

> **P2 实现记录（实现时回填，先记录再改代码）**
> - **偏离①（候选协议锁死）**：方案未覆盖 `candidate_protocols` 的续写锁死。
>   原代码在 `continuation_profile_key` 存在时把候选协议锁为
>   `ChatCompletions→[ChatCompletions]` / `Responses→[Responses]` /
>   `Messages→[]`。`Messages` 场景下候选协议为空数组，逃生轮连候选都没有，
>   逃生必然无效。实现时把 `candidate_protocols` / `candidate_passes` 改为可变并在
>   逃生时按 `responses_route_strategy` 的 NoReplay 分支语义重建（ProtocolAgnostic →
>   [native, opposite]，Responses → [Responses]，ChatFallback → [ChatCompletions]）。
>   这样 Messages 场景逃生后也能放开到 endpoint 的原生/对侧协议。
> - **决策②（能力集不重算）**：逃生轮**不**重新计算 `requested_features` /
>   `route_capability_cache`。理由：净化只剥字段、不删 item（不变量 6 要求文本逐条保留），
>   历史里的 `reasoning` item 仍会被 `scan_responses_reasoning` 扫到，`ReasoningOutput` /
>   `ReasoningReplay` 仍是 required；因此逃生目标路由必须支持与原始请求相同的能力集，
>   这与方案正文一致（逃生只解决「路由不可用」，不解决「能力不匹配」）。
> - **决策④（chat-only fallback 续写不逃生，2026-08-22 全量回归发现）**：逃生 GREEN 后
>   `tests/gateway/responses/fallback.rs::chat_only_fallback_loads_exact_continuation_before_candidate_failover`
>   失败（断言 729 `is_server_error` / alternative_hits == 0）。原因：P2 逃生把续写请求打到
>   fallback alternative（ChatCompletions，priority 1）。该用例是 chat-only fallback 功能的既有契约：
>   续写契约 pin 在 exact 路由，即使 exact 503 也**不** failover 到 alternative（避免打
>   非 continuation-aware 兜底路由），直接 503 交还客户端。方案正文未覆盖此场景。
>   **实现决策**：逃生判定增加 `&& !chat_only_responses_fallback`（该变量在
>   `process_gateway_request_inner` 顶层 5093 行定义：
>   `endpoint == Responses && eligible_responses_routes == 0 && eligible_chat_routes > 0`）。
>   纯 chat-only 池（responses 路由数为 0）的续写由 fallback 机制管辖，逃生不越界；
>   P2 主用例（Responses 协议池）不受影响。备选（Rejected）：更新 fallback 测试断言允许
>   逃生打 alternative——会推翻上一功能明确契约（该测试命名即
>   "loads_exact_continuation_before_candidate_failover"），且 alternative mock 语义
>   就是 "wrong route"，客户端会收到错误内容，拒绝。
> - **决策③（details 字段）**：`continuation_pin_escaped` 在 P2 一并落地
>   （P3 计划里还有 `continuation_pinned` / `continuation_candidate_count` 两个字段）。
>   P2 RED 的「逃生也失败」用例断言该字段，为避免 P2 测试挂到 P3 才绿，P2 就把这一个字段
>   加进 `terminal_route_failure_error` 的 details（参数 +1），P3 再补另两个。


### P3 终态可观测性

- [ ] RED：断言终态 details 新增三个字段。
- [ ] GREEN（`src/server/gateway/errors.rs` + `gateway.rs` 的 routes_exhausted 日志）：
  - `continuation_pinned: bool`（本次请求是否受续写契约约束）；
  - `continuation_candidate_count: usize`（契约过滤后的候选路由数）；
  - `continuation_pin_escaped: bool`（是否已经逃生过）。
- [ ] 理由：这次排查最大的成本就是「从错误里完全看不出候选池被削到 1 条」。

### P4 单候选场景不再自我升级冷却（治 R3）

- [ ] RED：`tests/route_health.rs` —— 同一路由**跨请求**连续失败，但每次都是
      「该请求的唯一候选」时，step 不应逐次升级到 max。
- [ ] GREEN：两选一（实现者判断后在本文记录选择与理由）：
  - (a) `RouteOutcome::RouteFailure*` 增加 `sole_candidate: bool`，为真时按
    `repeat_within_request` 的既有语义处理（重置冷却起点、不升级 step）；
  - (b) 不改健康层，改在 gateway 侧：当候选集大小为 1 且受续写约束时，
    传 `repeat_within_request = true`。
  - **注意**：(b) 改动小但语义被复用，需在注释里写清楚；两者都必须同时覆盖本地与 Redis 后端。

### P5 能力探测覆盖率可见性（治 R2）

- [ ] GREEN：管理页/诊断接口暴露「每个 (上游, key) 的档案状态」，把
      `DialectProfileState::Unknown` 的 key 高亮为「无法承接续写」。
      优先做只读展示，不做自动探测（自动探测有配额风险）。
- [ ] 若后端已有等价接口（先 grep `capability_admin.rs` 的档案列表接口），只补前端展示即可。

### P6 前端设置项 + 部署文档

- [ ] `frontend/src/types/index.ts` + `frontend/src/utils/runtimeSettings.ts`：
      group `routing`，两个设置项：
      ① label「续写路由逃生」（P1 的开关），说明：
      「当上次成功的那条路由不可用时，允许把会话历史净化后转移到其它可用路由；
      关闭后该会话只能等原路由恢复」。
      ② label「本地并发租约上限（秒）」（P7 的 `upstream_local_lease_ttl_seconds`），
      说明：「兜底回收未正常归还的并发槽；小于单次请求最长时长会误回收，勿低于流最大时长」。
- [ ] `DEPLOYMENT.md`：在排障小节补第 1 节的判据表与第 3 节的立即缓解。

---

### P7（2026-08-22 新增，P0）本地并发租约的兜底回收（治 R5） —— ✅ commit `be86806`

> 这条**独立于续写 pin**：即使 P2 不做，它也能让「空闲后依然报错」的会话在一个租约 TTL 后自愈。
> 优先级建议排在 P2 之前落地（改动更小、风险更低、收益立竿见影）。

- [x] RED：`tests/upstream_concurrency.rs`（或既有并发用例文件）新增
  - **泄漏回收**：手工构造一条永不归还的租约（直接调 `try_reserve_upstream_account_request`
    后 `std::mem::forget` 掉 guard，或用测试钩子），推进时间超过 TTL 后，
    同账号的新请求**必须**能拿到槽位；
    → `leaked_lease_is_reclaimed_after_local_ttl`（`#[tokio::test(start_paused = true)]`，
    泄漏后 `advance(3661s)` 前 RED 实测 panic：仍 `LocalConcurrency`）
  - **正常路径不受影响**：一个长流请求（超过 TTL）在运行期间其槽位**不得**被回收
    —— 因此必须有续租（见下），仅靠 TTL 会误杀长流；
    → `long_stream_lease_is_renewed_before_ttl_expiry`（半 TTL 后续租，越过原 TTL 槽位仍在）
  - `in_flight` 快照在回收后归零。
    → 两用例均断言 `in_flight == 0` 与 `leaked_reclaimed_total == 1`
- [x] GREEN（`src/state.rs`）：给本地后端的 `active_leases` 补上与 Redis 对等的生命周期语义：
  1. `HashSet<String>` → `HashMap<String, Instant>`（lease_id → 到期时间），
     到期时间 = `now + upstream_lease_ttl`；→ `src/state.rs:6393-6400`（`active_leases` 类型）
  2. 每次 `try_reserve_upstream_account_request` / `try_reserve_upstream_account_hedge`
     在计数**之前**先剔除已过期的条目（惰性回收，和 Lua 的 `ZREMRANGEBYSCORE` 同构，
     不需要后台任务）；→ `prune_expired_upstream_leases`（`src/state.rs:6402-6420`），
     在两个 reserve 路径（`src/state.rs:3627`、`src/state.rs:3713`）与
     `active_upstream_lease_count`（`src/state.rs:6422`）里调用
  3. **续租**：长流请求必须在运行期间刷新到期时间，否则会被误回收。
     先 grep `lease_renew.lua` / `renew` 的现有调用点，本地后端接同一个续租入口；
     若本地后端目前没有续租路径，则 TTL 必须取 `max(upstream_stream_max_duration_seconds, …)`
     这类安全值，并在文档里写明取舍（**实现者必须二选一并在本文记录**）；
     → **选 Option A（实现续租）**。依据：
     `upstream_stream_max_duration_seconds` 默认 86400，安全 TTL 会退化成约等于不回收，
     与 P7 目的矛盾；`gateway.rs` 已有 `DownstreamConcurrencyGuard::renew_if_due`
     （`src/server/gateway.rs:2970` 一带，per-chunk 节流续租范本，续租间隔 = TTL/2）。
     实现：**本地分支**刷新 expiry（`AppState::renew_upstream_request`，
     `src/state.rs:3876-3902`）；**Redis 分支复用既有 `lease_renew.lua`**
     （`src/state/redis_runtime/lease_renew.lua`，ARGV = lease_id + lease_duration_ms，
     与 `renew_downstream_lease` 同一脚本、幂等返回 0 即为 no-op——重命名后的入口
     `RedisRuntimeCoordinator::renew_upstream_request`，`src/state/redis_runtime.rs:1288-1321`）。
     调用点：`UpstreamRequestReservation::renew_if_due`（`src/server/gateway.rs:3158-3196`），
     主 body 循环每 chunk 调一次（`stream.rs:791`、`stream.rs:1577`）。泄漏的 guard
     不再产出 chunk → 停止续租 → TTL 后惰性回收，不会形成永续租。
  4. 新增运行时设置 `upstream_local_lease_ttl_seconds`（默认 3600，范围 60..=86400，
     immediate），接线：types.rs（`DEFAULT_UPSTREAM_LOCAL_LEASE_TTL_SECONDS` :123 一带 +
     AppConfig :258-259 + default fn :1033）→ runtime_settings.rs（IMMEDIATE 字段表 :50 +
     结构体 :122-123 + from/apply :277/:358 + 校验 :505-507）→ main.rs
     （`UPSTREAM_LOCAL_LEASE_TTL_SECONDS` env + clamp，:100/:239）→ 前端设置项
     （见 P5/P6 一起接线；label「本地并发租约上限（秒）」，group `routing`）。
     注意：Redis 后端**不**吃这个设置（Redis lease TTL 仍是
     `(upstream_stream_max_duration_seconds+60)*1000`，与 Redis 原语义一致）。
- [x] GREEN（`src/server/gateway.rs`）：`spawn_release` 里
      `Handle::try_current()` 失败的分支目前只打日志就放弃（`gateway.rs:3057-3066`，
      现为 `src/server/gateway.rs:3057-3082`）。
      改为**同步兜底**：直接标记该 lease 为立即过期（配合上面的惰性回收），
      不依赖 Tokio 运行时。
      → `AppState::expire_upstream_request_lease_sync`（`src/state.rs:3904-3933`,
      `try_lock` 失败则回退 TTL 惰性回收）；调用点 `src/server/gateway.rs:3063-3081`
      （成功 warn「reclaimed synchronously」，失败 error「left for TTL reclamation」；
      Redis lease 只记日志，靠 Redis 原生 TTL 自愈）。
- [x] 观测：`in_flight` 之外，再暴露 `leaked_reclaimed_total`（本次惰性回收掉的过期租约数）
      到上游运行时快照，便于确认泄漏是否真实发生过。
      → `UpstreamRuntimeSnapshot` / `WithFeedback` 新增字段（`src/state.rs:6447`、`:6471`），
      本地为真实累计值，Redis parse 填 0（`redis_runtime.rs:2948`、`:2980`）。
- [x] 验证：`rtk cargo test --test upstream_concurrency --test gateway`；
      全量门 `cargo test --all` 1696 passed / 88 ignored（62 suites）；fmt/clippy 干净。
      字段计数断言同步 +1：`tests/runtime_settings.rs:154`（52→53）、
      `tests/admin_runtime_settings.rs:202`（53→54），并补 round-trip 与越界校验用例。

## 6. 测试矩阵

| 场景 | 期望 |
|------|------|
| pin 路由冷却，同组另一 key 健康 | 逃生成功，走另一 key（P2 主用例） |
| 逃生成功后再续写 | 直接命中新路由，不再逃生 |
| 历史含 encrypted reasoning | 逃生请求里被剥离，其余内容逐条保留 |
| pin 路由返回 400 | 不逃生，400 直接交还 |
| 全池都不可用 | 逃生后仍 `upstream_routes_exhausted`，details 标记 `continuation_pin_escaped: true` |
| 开关关闭 | 完全现状行为 |
| 单候选跨请求连续失败 | 冷却 step 不再顶到 max（P4） |
| 泄漏租约超过 TTL | 同账号新请求能拿到槽位，`in_flight` 归零（P7）✅ `be86806` |
| 长流请求运行期超过 TTL | 其槽位**不被**回收（续租或安全 TTL，P7）✅ `be86806`（选续租） |
| 回归 | `responses/history.rs`、`reasoning.rs`、`tools.rs`、`2026-08-08` 契约失败转移用例全绿 |

---

## 7. 风险与回滚

| 风险 | 缓解 |
|------|------|
| 逃生后模型丢失 reasoning 连续性，重新推理一次 | 这是刻意取舍：会话活着 > 推理连续；只在原路由不可用时发生 |
| 净化剥错字段导致上游 400 | 净化只剥「供应商绑定产物」白名单字段，测试逐条断言内容保留；出问题关开关即可 |
| 逃生让本次请求多花一轮 | 只在本来必然失败的路径上发生，净收益 |
| 与 V2 契约失败转移语义重叠 | 严格分层：契约转移优先，逃生是它耗尽后的最后一跳 |
| P7 的 TTL 误回收长流的槽位 → 同账号超卖并发 | ✅ 续租（`renew_if_due` 每 chunk）＋`long_stream_lease_is_renewed_before_ttl_expiry` 护法；TTL 可运行时调大（60..=86400） |

- **回滚**：`upstream_continuation_pin_escape_enabled = false`（immediate），或 revert commit。
- **Constraint:** 不改错误分类语义；不改 V2 契约派生逻辑；逃生每请求最多一次。
- **Rejected:**
  ① 直接把续写 pin 降级为软偏好（会让正常情况下的 reasoning 保真度无谓劣化）；
  ② 在客户端侧让 codex 丢弃 `previous_response_id`（我们控制不了客户端）；
  ③ 失败时删除 response 历史让下次「继续」变成新会话（会静默丢上下文，比 503 更糟）。
- **Confidence:** 根因链已代码核实（R1/R4 是硬事实）；R2/R3 是强推断，按第 1 节判据可现场自证。
- **Scope-risk:** high（触碰续写契约与主路由循环），故全程带开关。
