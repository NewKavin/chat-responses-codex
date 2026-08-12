# Codex 国产模型上游加固总体方案（协议保真 + 会话隔离 + 冷却治理 + 探测修复）

> **面向实施模型的说明**：本方案由深度代码分析产出，所有根因均带 `文件:行号` 证据（基于 main 分支，commit 3329210 之后的工作树）。请逐个 Workstream 按顺序实施，每个 Workstream 独立可交付、可回滚。实施时先写失败测试（RED），再改实现（GREEN），最后跑全量回归。行号可能随代码演进漂移，请以引用的函数名/字符串常量为锚点重新定位。

**日期**：2026-08-11
**状态**：待实施
**前置**：`docs/superpowers/plans/2026-08-11-gateway-reliability-and-probe-batches.md`（已完成）之上的下一轮修复。

---

## 0. 用户报告的六个问题与根因映射

| # | 用户症状 | 根因（详见对应 Workstream） | Workstream |
|---|---------|--------------------------|-----------|
| 1 | 2-3 个 Codex 并行就日常报 `all eligible upstream routes are temporarily unavailable: transient upstream server error (8 routes)` | 一个"请求形状"问题被当成 8 条路由的"服务故障"：同一个转换后的请求体在 8 个 key 上逐一重放、逐一 502、逐一冷却，共模故障打瘫整池 | B（止血）+ A（治本） |
| 2 | 一键探测思考档位，内网全部失败，deepseek-v4-flash 也不行 | 探测候选来自能力策略配置；内网旧部署 revision≥1 后**永远不会重新引导**内置策略，国模档位候选缺失 ⇒ 根本没发出档位探测请求；叠加判定只看 HTTP 状态码、批次耗时 30-60 分钟等次因 | E |
| 3 | 一旦 503，必须重启 Codex 才能恢复，上下文丢失 | 两条路径：(a) 继续会话能力门返回 **400**（客户端视为终结错误）；(b) 冷却时长（10s→5min 指数升级）远大于提示的 "try again in 1s"，客户端快速重试永远撞墙且半开失败继续加深冷却 | C |
| 4 | 报 503 时上游同请求直连 curl 是好的；一个 key 出错疑似殃及全部 | Key 级健康键本身是隔离的（见 §1.3），"全挂"不是共享状态而是共模失败：网关转发了国模/内网网关不接受的字段（`parallel_tool_calls`、`stream_options`、`metadata`、`user` 默认**不**剥离），上游/边缘代理 5xx，被一刀切归类 TransientServer | A + B |
| 5 | `stream disconnected before completion: [gateway_protocol_capability_unsupported] ... ParallelToolCalls` | 新请求已把 ParallelToolCalls 降为 optional（commit 5d2398f），但**继续会话状态里持久化的 required_capabilities 是旧集合**，每轮对话被原样注入 required，能力门 400 | D |
| 6 | 多个 Codex 窗口执行不同任务会串数据 | response_id 优先采用**上游返回的 id**（内网网关常返回非唯一 id），response_history 仅以 response_id 为唯一键且无下游隔离，upsert 直接互相覆盖 | F（正确性最高优先） |

**终极目标**：任意 OpenAI 兼容 chat-completions 上游（deepseek/GLM/minimax/未来国模）都能被 Codex（Responses API 下游）稳定使用；一条 key 故障不影响其他 key；会话不因网关错误而丢失。

---

## 1. 已确认的代码事实（实施前请复核）

### 1.1 错误分类：所有 5xx 一刀切 TransientServer
- `src/upstream_feedback.rs:406-412`（`classify_nonsemantic_default`）：`(500..600).contains(&status) → FailureClass::TransientServer`，不看 body 是否有服务故障证据。
- 502 的 body 若是 HTML（nginx 错误页），`StructuredError::parse`（`src/upstream_feedback.rs:40-50`）解析失败返回空，message 匹配不到任何语义 ⇒ 仍走 status 分支 ⇒ TransientServer。
- 对照：`FailureClass::RequestRejected` 在路由健康上按成功处理（`src/server/gateway.rs:570`：`Some(FailureClass::RequestRejected) => RouteOutcome::Success`）——分类对了就不会冷却，**问题全在"5xx 无法被识别为请求形状问题"**。

### 1.2 冷却参数与升级
- `src/state/types.rs:85-87`：Transient 冷却 base=10s、max=5min。
- `src/state/route_health.rs:1381-1397`（`route_cooldown`）：按 `consecutive_failures` 步进的指数退避 + 抖动；`route_health.rs:1369-1379`（`failure_step`）同类连续失败递增。
- `src/state/route_health.rs:1349-1360`（`route_failure_has_cooldown`）：TransientServer / Transport / RateLimited / CapacityUnavailable / ConcurrencySaturated / KeyQuota / ModelUnsupported 都会冷却。

### 1.3 路由循环与"全池打瘫"机制
- `src/server/gateway.rs:4930`（`'routing_rounds`）→ `:4963`（`'candidate_passes`）→ `:5313`（`'key_candidates` 遍历每个 key）。
- 候选 key 逐个尝试；上游失败分类后写路由健康。健康键 `route_health_keys(&upstream, &key_fingerprint, &runtime_model_slug, protocol)`（`gateway.rs:5317-5322`）——**按 (upstream, key指纹, model, protocol) 四元组隔离，key 之间无共享健康状态**。
- 冷却中的路由：`RouteAvailability::Cooling` → `record_cooled_route_attempt` → `last_error = "all eligible upstream routes are temporarily unavailable"`（`gateway.rs:5382-5398`）。
- 提示语组装在 `src/server/gateway/errors.rs:134-145`（含 `"gateway already retried for {:.1}s across {n} routing rounds"`）。
- **级联机制**：一个下游请求在 key A 上游 502 → A 冷却 → 循环继续 key B，**重放同一个转换后的请求体** → 同样 502 → B 冷却 → … 一次请求即可把 8 个 key 全部写入冷却；随后所有下游请求撞 `Cooling` 直接 503。

### 1.4 请求净化默认弱
- `src/server/gateway/compat.rs:69-119`（`normalize_chat_payload_for_upstream_compatibility`）：无条件移除 `service_tier`/`prompt_cache_key`/`store`/`verbosity`/`text` 等；但 `metadata`/`user`/`parallel_tool_calls`/`stream_options` 仅当 `strip_unknown_nonstandard_fields=true` 才移除（`compat.rs:92-96`）。
- 该开关来自 `upstream.strip_nonstandard_chat_fields`（`src/server/gateway/upstream.rs:1804-1817`），**默认 false**（`src/state/types.rs:368`）。
- 更强的能力驱动净化 `normalize_chat_payload_for_capabilities_with_requested_effort`（`compat.rs:121-202`）依赖 `resolved`（探测/策略产物）；内网探测全失败 ⇒ resolved 缺失 ⇒ 净化退化。**探测失败与 502 风暴互为因果。**

### 1.5 能力门与继续会话契约
- 预路由能力门：`src/server/gateway.rs:4721-4794`。`required_route_available=false` 时返回 **400** `gateway_protocol_capability_unsupported`，文案 `selected routes cannot preserve required capability {name}`（`gateway.rs:4762-4770`）。
- 报错能力名的取法（`gateway.rs:4747-4761`）：继续会话 profile 的 `failed_capability` → 否则 **capability 缓存中任意一条路由的 failed_capability**（`.values().filter_map(|route| route.failed_capability).next()`，BTreeMap 迭代序，与本请求 required 无必然关系）→ 否则 required 集合第一个。**误导性命名的来源。**
- 新请求 ParallelToolCalls 已是 optional：`src/server/gateway/capability_routing.rs:406-414`（测试锚点 `capability_routing.rs:1417`、`:1438`）。
- 但继续会话状态 `GatewayContinuationState` 持久化 `required_capabilities`（`capability_routing.rs:49`、契约 `:55-65`），并在 `apply_to_requested`（`capability_routing.rs:173-179`）把存量集合 **extend 进本轮 required**。旧版本创建的会话（当时 ParallelToolCalls 是 required）每一轮都重新要求它 ⇒ 问题 5 在修复后仍复现的路径。V1 契约走 `LoadedContinuation::V1NeedsDerivation`（`capability_routing.rs:74-124`）。

### 1.6 探测管线
- 计划构建 `probe_plan_for_route`（`src/server/gateway/capability_probe.rs:180-260`）：`ReasoningControl` 案例**只来自** `configuration.probe_candidates_for(route)`（策略配置），基础计划里没有任何档位案例（`ProbePlan::full()` 仅 MinimalText/FunctionTools/ParallelTools/Image 等，`capability_probe.rs:144-177`）。
- 策略引导仅在 revision==0：`src/state.rs:4777-4779`（`capability_policy_bootstrap_on_zero && revision == 0`），开关默认 true（`src/state/types.rs:223`、`src/main.rs:143`）。内置策略含国模档位候选（`templates/capabilities/current-deployment.example.json`，含 `domestic-deepseek-family`/`domestic-glm-5-family`/`domestic-minimax-family` 与 `deepseek-v4-flash` 专条）。**旧部署 revision≥1 ⇒ 永不引导 ⇒ 档位候选缺失 ⇒ 一键探测根本不含档位案例。**
- 档位判定只看状态码：`capability_probe.rs:1719-1748`（`ReasoningControl` 分支 `verdict_for_status(status, "reasoning_control_accepted", ...)`）。**国模普遍忽略未知字段返回 200**：会"假接受"；内网边缘 5xx 则记 OperationalFailure。判定不检查 `reasoning_content`/reasoning tokens 证据。
- 探测作业执行 `run_probe_job`（`capability_probe.rs:623-668`）；配置指纹不一致时**静默丢弃**（`:635-637`）；批次并发 2、单请求超时为 `capability_probe_request_timeout_seconds`，130 路由全量一轮 30-60 分钟（commit 78be1b7 注）。
- 400 方言错误已能触发补充探测 `maybe_queue_dialect_error_probe`（`capability_probe.rs:561-621`），**但只认 status==400**（`:574`），502/422 不触发。

### 1.7 会话串数据
- 流式转换器采用上游首个 chunk 的 `id` 作为下游 response_id：`src/protocol.rs:2547-2554`（`initialize_metadata`：`event.get("id") ... or_else(|| resp-{uuid})`）。
- 非流式：`src/protocol.rs:673`：`input.get("id").and_then(Value::as_str).unwrap_or("resp")` —— 上游无 id 时**常量 "resp"**。
- 历史存储仅以 response_id 为键、无下游维度：`src/state.rs:722-758`（`store_response_history`，内存 insert + PG `upsert_response_history`），查询 `src/state.rs:760-788`（`response_history(response_id)`）。
- 继续会话解析：`src/server/gateway.rs:3289-3340`（`previous_response_id` → `state.response_history(id)`，未命中报 `unknown previous_response_id`，`gateway.rs:3313`）。
- **串数据成立条件**：内网 OpenAI 兼容网关（one-api/new-api/vLLM 类）返回低熵/重复/缺失 id ⇒ 两个窗口的响应落到同一个 response_id ⇒ upsert 互相覆盖 ⇒ 下一轮 `previous_response_id` 取到对方的对话历史。**同时这也是越权读取隐患：任何下游 key 可读任意 response_id 的历史。**

---

## 2. Workstream F：会话隔离与 ID 治理（正确性问题，最先做）✅ 已完成 (commit c328712)

### F1. 下游 response_id 永远由网关生成
**改动**：
- `src/protocol.rs` `initialize_metadata`（锚点 `:2547`）：response_id 一律 `format!("resp_{}", Uuid::new_v4().simple())`，**不再采用上游 event id**；上游 id 存入新字段 `upstream_response_id: Option<String>`，仅写诊断日志/usage 表。
- `src/protocol.rs:673` 非流式路径同理：删除 `unwrap_or("resp")` 与上游 id 采用，统一走网关生成。
- 检查所有引用下游 response id 的 SSE 事件（`response.created`/`response.completed`/output_item 等）使用同一个生成值（`response_id_value`，`protocol.rs:2571-2579` 已有缓存语义，保留）。
- 注意 Codex 对 id 前缀无强约束，但保持 `resp_` 前缀与 OpenAI 形态一致。

**测试**：
- 单元：两个模拟上游流都返回 `id:"chatcmpl-same"` → 两个转换器产出的下游 response_id 互不相同且均以 `resp_` 开头。
- 单元：上游 chunk 无 `id` 字段 → 仍生成唯一 id（回归 `"resp"` 常量 bug）。

### F2. response_history 增加下游隔离维度
**改动**：
- `src/state.rs` `store_response_history` / `response_history`（锚点 `:722`/`:760`）：签名增加 `downstream_key_id: &str`；内存 `ResponseHistoryStore` 复合键 `(downstream_key_id, response_id)`。
- PostgreSQL：迁移脚本给 response_history 表加 `downstream_key_id TEXT NOT NULL DEFAULT ''` 列 + `(downstream_key_id, response_id)` 唯一索引；`upsert_response_history`/查询带上该列。旧行（默认 ''）：查询未命中新键时回退查旧键**一次**并在命中后迁移写回新键（兼容存量会话，不破坏用户现有上下文）。
- 调用点：`src/server/gateway.rs:3305-3313` 解析 `previous_response_id` 时传入当前 `downstream.id`；所有 `store_response_history` 调用点同步。
- 未命中时错误信息保持 `unknown previous_response_id`（避免泄露他人 id 是否存在）。

**测试**：
- 集成：下游 key A 存的 response，key B 用同 id 查询 → 未命中。
- 集成：两个并发请求（同 key、不同窗口）+ 上游重复 id → F1 保证 id 不同 → 历史互不覆盖；断言各自 `previous_response_id` 回放的 items 与本窗口一致。
- 回归：旧行（无 downstream 列）在新代码下仍可被原 key 读到。

### F3. 共享状态审计（防同类隐患）
**改动**：审计并修正以下位置是否存在"以上游 id/model/请求哈希为键、无请求隔离"的缓存：
- 继续会话状态的存取键（`GatewayContinuationState` 随 response_history 的 `request_state` 存储——F2 覆盖）。
- usage 日志、troubleshooting 快照中以 response_id 关联的行（加 downstream 维度或容忍）。
- 任何 `Arc<Mutex<...>>` 的流式累积器必须是 per-request 构造（转换器本身 per-request，确认无池化复用）。

**验收（对应问题 6）**：双窗口 soak（同一下游 key、两个 Codex 实例、交错多轮工具调用 30 分钟）零串话；PG 中 response_history 行均带 downstream_key_id。

---

## 3. Workstream B：错误分类与冷却治理（止血，与 F 并行先行）

### B1. 5xx 细分：无证据 5xx 不再直接全额冷却 ✅ 已完成 (commit 8bb4310)
**改动**（`src/upstream_feedback.rs`）：
- 新增语义：`UpstreamResponseSemantic::EdgeProxyError`（HTML/空 body 的 502/503/504，典型 nginx 网关错误）。识别：body 非 JSON 且（以 `<` 开头或包含 `<html`/`bad gateway`/`gateway time-out` 等），或 Server/Content-Type 头指示代理。
- `classify_nonsemantic_default`（锚点 `:406`）拆分 5xx：
  - 500/502 + body 含请求语义证据（`invalid`/`unsupported`/`field`/参数名等，复用 `message_is_request_rejected`、`message_is_feature_unsupported`）→ `RequestRejected`/`FeatureUnsupported`（不冷却/短隔离）。
  - EdgeProxyError → 新 FailureClass 或复用 TransientServer 但标记 `uncertain`，冷却 base 降为 2-3s 且 `failure_step` 不随之升级（新增 `RouteFailureClass` 变体时同步 `route_health.rs:1349` 与 `:1381` 两个 match）。
  - 有明确服务故障证据（JSON error 且 message 含 `internal`/`overload`/繁忙类）→ 维持 TransientServer 现行为。
- 保持既有显式语义（并发/上下文溢出/目标模型容量）优先级不变（`classify_upstream_response`，`upstream_feedback.rs:510-543`）。

**测试**（`tests/unit/upstream_feedback.rs` 追加）：
- 502 + HTML body → EdgeProxyError 语义、短冷却类。
- 502 + `{"error":{"message":"invalid parameter: stream_options"}}` → RequestRejected（不冷却）。
- 500 + `{"error":{"message":"internal server error"}}` → TransientServer（维持）。

### B2. 请求内共模失败熔断（防"一个请求打瘫整池"，本 Workstream 核心）✅ 已完成 (commit 65f8005)
**改动**（`src/server/gateway.rs` 路由循环，锚点 `'key_candidates` `:5313`）：
- 在单个下游请求的作用域内维护 `common_mode_tracker: (FailureClass, Option<u16>, 计数)`。
- 当**连续 K=2 条不同路由**以相同 (class, upstream_status) 失败且 class ∈ {TransientServer, EdgeProxyError, RequestRejected 疑似}：
  1. 停止尝试剩余路由（不再消耗/污染健康状态）;
  2. 本请求已写入的冷却改记为 `UncertainRouteFailure`（已有 `RouteOutcome::UncertainRouteFailure`，`gateway.rs:2790` 锚点）或直接回滚（route_health 提供 `revert_last_failure(route_key, token)` 新 API）;
  3. 返回给下游的错误语义改为"上游拒绝了该请求（疑似请求形状）"，HTTP 502/400 视 class，附 troubleshooting（含首个上游错误摘要），**不是** all-routes-unavailable 503。
- K 可通过 runtime settings 配置（默认 2；设 0 关闭熔断）。
- 同时把该请求样本送 `maybe_queue_dialect_error_probe` 的扩展入口（见 A3）。

**测试**（`tests/gateway/`）：
- 模拟 8 条路由全部对同一请求回 502+HTML：断言只物理尝试 2 条、无任何路由进入冷却、下游收到带上游摘要的 502；随后另一个正常请求立即可用（路由未被污染）。
- 模拟仅 key1 故障（502），key2 正常：断言 key1 冷却、key2 成功——**保持问题 4 要求的 key 级隔离**（已有测试 `fe1c160` 锁定，勿回归）。

### B3. 冷却/半开与重试提示对齐 ✅ 已完成 (commit 15372a0)
**改动**：
- 半开探测失败不无限加深：`failure_step`（`route_health.rs:1369`）当 `state.last_failure_class` 相同且处于半开验证时，step 封顶（如 max 5），防止 5min 顶格常驻。
- `errors.rs:134` 的 `please try again in {n}s` 改为取**所有被记录冷却路由的最小剩余到期时间**（`record_cooled_route_attempt` 已带 `retry_after`，聚合取 min），并在 503 响应头附 `Retry-After: <同值>`。
- routing_rounds 的等待预算（现 ~7s/3 轮）：当 min 冷却到期 ≤ 可配置的 `gateway_max_wait_seconds`（默认 30s，runtime setting）时，循环内 sleep 到期后重试而不是提前放弃——2-3 并发的开发场景宁可多等几秒也不要把错误抛给 Codex。

**测试**：冷却 10s 场景下请求在 ~10s 内自动成功返回（不重启客户端）；503 响应带准确 Retry-After。

---

## 4. Workstream C：会话不重启恢复（问题 3） ✅ 已完成 (commit 5df3e4b)

### C1. 存量继续会话 required 集合按当前策略消毒
**改动**（`src/server/gateway/capability_routing.rs`）：
- 新函数 `sanitize_stored_required(required: &mut BTreeSet<Capability>)`：移除当前版本定义为"可降级/optional"的能力（至少 `ParallelToolCalls`；表驱动，常量列表 `DOWNGRADEABLE_STORED_CAPABILITIES`），被移除的转入 optional。
- 在 `apply_to_requested`（锚点 `:173-179`）extend 前调用；`LoadedContinuation::V1NeedsDerivation` 的派生路径同样过滤。
- 契约版本无需升级——消毒是读取时行为。

**测试**：构造带 `ParallelToolCalls` 于 required 的 V1 与 V2 继续会话状态，下一轮请求断言 requested.required 不含它、optional 含它，且请求正常路由（不 400）。**这是问题 5 的直接回归测试。**

### C2. 能力门在继续会话场景不再 400 终结
**改动**（`src/server/gateway.rs:4721-4794`）：
- `required_route_available=false` 且存在继续会话时，先尝试降级路径：对 `DOWNGRADEABLE_STORED_CAPABILITIES` 逐个从 required 摘除重算 `required_route_available`；成功则记录 downgrade 并继续路由。
- 仍不可用时：若原因是路由**暂时**不可用（冷却/容量）→ 返回 503 + Retry-After（可重试，Codex 不丢会话）；仅当确为能力/配置永久不满足 → 维持 400，但 capability_name 修正（C3）。

### C3. 修正误导性的能力名
**改动**（`gateway.rs:4747-4761`）：`capability_name` 不再从"缓存任意路由的 failed_capability"兜底（删除 `.values().filter_map(...).next()` 分支）；改为：与本请求 required 集合求交集的 failed capability，否则列出 required 集合本身。

**验收（对应问题 3）**：拉闸模拟上游 30s 故障后恢复，运行中的 Codex 会话在不重启的情况下：期间请求收到带 Retry-After 的 503 或在等待预算内自动成功；故障恢复后下一轮对话直接成功；全程无 400。

---

## 5. Workstream D：ParallelToolCalls 残留清理（问题 5） ✅ 已完成 (commit 5df3e4b)

- D1 = C1（消毒），D2 = C3（命名），已覆盖。
- D3. 确认发送路径全部剥离：`strip_unsupported_parallel_tool_calls`（`compat.rs:204-214`）在**所有**构建上游请求的路径被调用，包括继续会话回放与失败重试路径（检索 `upstream.rs` 全部 body 组装点）；对 `resolved` 缺失（未探测）的路由按"不支持"处理（保守剥离，配合 A1）。
- D4. 中途流失败后的重选路由（stream 内 failover，锚点 `src/server/gateway/stream.rs:2176` `selected route cannot preserve required protocol capability`）使用与预路由门一致的消毒后 required 集合，不得使用契约原始集合。
- **测试**：旧契约会话 + 全部路由不支持 ParallelToolCalls → 请求成功（字段被剥离），SSE 正常完成；中途 failover 场景同样不因该能力 400/断流。

---

## 6. Workstream E：一键探测思考档位修复（问题 2）

### E1. 内置策略可升级（去除 revision==0 死锁） ✅ 已完成 (commit 2e50dcd)
**改动**：
- `CapabilityConfiguration` 增加 `builtin_policy_version: u32`（内置模板中维护递增版本）。
- 启动时（`src/state.rs:4777` 区域）：除 revision==0 全量引导外，新增**合并引导**——当嵌入模板的 `builtin_policy_version` > 已存配置记录的值：把模板中 `id` 以 `domestic-`/内置前缀开头且现存配置中不存在的 policy/expectation 条目**追加**（绝不覆盖或删除 operator 自建条目），随后 revision+1、记录新 builtin 版本。这保持了既有约束"operator-managed nonzero policy 不被覆盖"，同时让新内置国模条目可达存量部署。
- 管理端新增 `POST /api/admin/capabilities/policy/rebootstrap`（确认式）：显式用内置模板做同样的合并（或带 `mode=replace` 的全量重置，双重确认），前端在探测页给入口与结果 diff 展示。

**测试**：构造 revision=3、无 domestic 条目的存量配置 → 启动后 domestic 条目存在、operator 条目原样；再次启动幂等。

### E2. 档位判定用响应证据，不只状态码 ✅ 已完成 (commit 0e57dc2)
**改动**（`capability_probe.rs` `ReasoningControl` 分支，锚点 `:1719`）：
- 请求体加最小推理诱导 prompt（现 `PROBE_INPUT_PROMPT` 可保留，必要时换成需一步推理的算术题）。
- 判定三态：
  - `accepted_verified`：HTTP 200 且响应含推理证据——`choices[0].message.reasoning_content` 非空，或 `usage.completion_tokens_details.reasoning_tokens > 0`，或（GLM 风格）`message.reasoning` 字段；
  - `accepted_unverified`（新增 evidence code `reasoning_control_ignored`）：200 但无任何证据——**不得**据此把档位标为 verified（多数国模忽略未知字段返回 200，这是现在"假接受/假档位"与"全不可得"并存的根源）；
  - `rejected`/`failed`：现行为。
- `apply_probe_outcome` 侧仅 `accepted_verified` 写入 `reasoning_controls` 生效档位（`src/capabilities/profile.rs:192-197` 锚点）。
- 对 chat 协议同时探测方言变体：当策略候选包含 `thinking` 字段（GLM `thinking:{"type":"enabled"}` 对象形态），`ReasoningControl` 需支持非字符串 value（value 类型从 `String` 扩为 `Value`，模板同步）。

**测试**：模拟上游 A（200+reasoning_content）→ verified；模拟上游 B（200 无证据）→ ignored 且档位不写入；模拟上游 C（400 unsupported）→ rejected。

### E3. 批次执行体验 ✅ 已完成 (commits 75bbe85, ec9e8c6, cbefdb9, 738dfc3)
**改动**：
- 探测并发从固定 2 改为 runtime setting `capability_probe_concurrency`（默认 4，按账号 key 并发预算封顶），批次预计剩余时间在 `GET /api/admin/capabilities/probe-batches/{id}` 返回并前端展示。
- 冷却中的路由：探测跳过时记 evidence `skipped_route_cooling` 并在 UI 单独归类（现在笼统的失败让用户误判"全失败"）。
- 配置指纹变化导致的静默丢弃（`capability_probe.rs:635-637`）：改为记 `superseded` 状态并计入批次终态，不再无声消失。
- **探测范围收敛（用户明确需求）**：一键探测默认只覆盖"下游实际可见的模型"（有下游 key 映射/白名单内的 exposed model），不再对全部 130 路由全量探测；UI 提供模型多选框（默认勾选下游关注集合，可手动增删），后端 `POST /api/admin/capabilities/probe-batches` 接受 `models: Vec<String>` 过滤参数（空 = 旧行为全量，向后兼容）。批次状态返回中标注本次探测的模型清单。

### E4. 探测与正式流量的一致性 ✅ 已完成 (commit d41bc29)
**改动**：探测请求体构造走与正式请求相同的净化管道（`normalize_chat_payload_for_upstream_compatibility` + 方言预设，见 A2），确保"探测通过 ⇒ 正式请求同形状"；在 ProbeExecutor（`capability_probe.rs:653-667`）构造处复用。

**验收（对应问题 2）**：在模拟内网环境（存量 revision≥1 + deepseek-v4-flash 上游桩，返回 reasoning_content）跑一键探测：档位在批次完成后显示为 verified 集合非空；GLM thinking 对象形态可被探出。

---

## 7. Workstream A：协议转换保真与方言层（治本、终极目标）

### A1. 未验证路由默认保守净化 ✅ 已完成 (commit 07709c1)
**改动**：
- `upstream.strip_nonstandard_chat_fields`（`types.rs:322`）由 bool 改三态 `NonstandardFieldPolicy { Auto, AlwaysStrip, Forward }`，**默认 Auto**（序列化兼容旧 bool：false→Auto，true→AlwaysStrip）。
- Auto 语义（在 `upstream.rs:1804-1817` 调用处实现）：该路由存在 `resolved` 能力档案 → 按档案（现行 `normalize_chat_payload_for_capabilities_with_requested_effort`）；**无档案（未探测/探测失败）→ 按 AlwaysStrip 集合剥离**（`metadata`/`user`/`parallel_tool_calls`/`stream_options` + `compat.rs:135-147` 的 omit 集合）。`stream_options.include_usage` 例外：仅当流式且需要 usage 时保留尝试，失败样本交 A3 学习。
- 前端上游表单暴露三态选择（默认 Auto，带说明）。

**测试**：无档案路由的出站 body 不含上述字段；有档案且声明支持的路由保留 `parallel_tool_calls`。

### A2. 上游方言预设（探测缺失时的静态兜底） ✅ 已完成 (commit f44dfc7)
**改动**：
- `UpstreamConfig` 增加 `dialect_preset: Option<String>`（`openai`/`deepseek`/`glm`/`minimax`/`generic-strict`），前端下拉。
- 预设编译为一个静态 `ResolvedCapabilities` 底座（`src/capabilities/types.rs:470` 区域增构造器）：如 `glm` → `reasoning_control_field=Some("thinking")` + 对象值 effort_map、剥离 `stream_options`；`deepseek` → `reasoning_content` carrier、`reasoning_effort` 直传；`generic-strict` → 全剥离。
- 合成顺序：探测 resolved（最优）> 方言预设 > Auto 保守剥离。实现于 route 能力解析入口（`src/capabilities/resolver.rs`，与 `evaluate_route_capabilities_with_runtime_hints` 汇合处）。
- 模板 JSON 的 policy 亦可引用预设，减少重复。

### A3. 失败驱动的形状学习闭环 ✅ 已完成 (commit 33c962c)
**改动**：
- `maybe_queue_dialect_error_probe`（`capability_probe.rs:561`）触发条件扩展：除 400 外，纳入 B1 识别出的"5xx+请求语义证据"与 B2 共模熔断样本（把首个上游错误文本传入）。
- 新增受控的**同请求降级重试**：当首条路由失败且错误文本命中方言字段列表（`capability_probe.rs:591-604` 的清单），在**同一条路由**上进行一次剥离该字段后的重试（`same_route_retry_attempted` 机制已有锚点，`gateway.rs:5405`），成功则记录 runtime hint（`RuntimeCapabilityHintSnapshot`，`gateway.rs:153` 已有钩子）供后续请求直接生效。
- 命中的 hint 异步落盘到该路由 profile（避免每次都试错）。

**测试**：上游对 `stream_options` 回 400/502（两种各测）→ 首路由内一次降级重试成功、无路由冷却、hint 生效后第二个请求首发即不带该字段。

### A4. 反向流转换鲁棒性（国模 SSE 兼容清单） ✅ 已完成 (commits 85c475f, 278d546)
**改动**（`src/protocol.rs` chat→Responses 流转换器区域）：
- 容忍并跳过非 JSON keepalive 行/注释行（`: ping`）；
- 最终 usage-only chunk（`choices:[]` 仅 `usage`）正确并入 `response.completed.usage`；
- `delta.tool_calls` 多个 index 交错累积的正确性（并行工具调用回传路径）与缺 `index` 时按 id 归并；
- `finish_reason: tool_calls/stop/length/content_filter` → Responses `status`/`incomplete_reason` 映射表齐全；
- `delta.reasoning_content`（deepseek/GLM）→ reasoning item/summary 事件（已有 ReasoningCarrier::ReasoningContent 机制，补齐流式增量事件顺序：`response.reasoning_*` 先于文本）。
- 每项配 SSE 夹具测试（用真实国模抓包样本入 `tests/fixtures/`）。

**验收（对应终极目标）**：新增"方言矩阵"集成测试：{deepseek 桩, GLM 桩, minimax 桩, 严格模式桩} × {纯文本, 单工具, 并行工具, 推理+工具, 长上下文} 全绿；Codex 实机对四类桩各完成一次多轮工具会话。

---

## 8. Workstream G：交付验证与部署

1. **Phase 0（实施前）**：确认内网部署镜像包含 2026-08-11 全部修复（`3329210`）；采集一次线上 `all eligible ...` 发生时的网关日志与对应上游错误 body 样本，验证 §1 根因假设（尤其 502 的 body 形态），把样本固化为测试夹具。
2. 单测/集成全绿：`rtk cargo test`、`rtk cargo clippy --all-targets -- -D warnings`、`rtk npm --prefix frontend test -- --run`、type-check、build。
3. 双窗口串话 soak（F 验收）+ 2-3 Codex 并行 30 分钟 soak（B/C 验收：零 all-routes-unavailable、零重启恢复）+ 探测验收（E）+ 方言矩阵（A）。
4. 分 Workstream 独立 commit/分支，按 **F → B → C/D → E → A → G** 顺序合入；每步可单独回滚。
5. 部署走仓库脚本（`scripts/build-package-image.sh` / `scripts/deploy.sh`），沿用既有发布证据流程。

## 9. 明确不做（避免实施跑偏）

- 不引入"按 base_url 共享健康状态"（用户明确要求 key 级隔离；现有隔离契约测试 `fe1c160` 不得回归）。
- 不做动态容量推断（既有设计已否决：transient 错误不是容量证据）。
- 不覆盖/删除 operator 自建能力策略条目（E1 只做增量合并 + 显式重置入口）。
- 不在网关内伪造推理档位（探不出就明示 unverified，不默认放行 xhigh 之类）。

## 10. 风险与回滚

| 改动 | 风险 | 缓解/回滚 |
|------|------|----------|
| B2 共模熔断 | 真·集体故障被误判为请求问题，重试变少 | K 可配置；熔断只在同 (class,status) 完全一致时触发；设 0 关闭 |
| F2 历史键迁移 | 存量会话短暂查不到历史 | 旧键回退读 + 迁移写；迁移脚本幂等 |
| A1 默认改保守剥离 | 个别上游依赖被剥字段（罕见） | 三态开关可回 Forward；per-upstream 配置 |
| E1 合并引导 | 与 operator 条目 id 冲突 | 仅追加不存在的内置前缀 id；启动日志列出新增条目 |
| C2 400→503 语义变化 | 客户端对 503 的重试节奏 | 带准确 Retry-After；Codex 对 503 自动重试友好 |
