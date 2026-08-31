# 交付给实施模型的提示词（复制以下全文）

---

你在 Rust 项目 `chat2Responses`（OpenAI Chat ↔ Responses 协议转换 + 多上游路由网关）中修一个**已完成根因定位**的生产故障。

**重要：根因已经闭环，证据链完整，不要重新排查、不要自行改变方向。** 你的任务是实施，不是诊断。
完整方案见 `docs/superpowers/plans/2026-08-25-route-exhaustion-cooldown-budget-invariant.md`，**动手前先完整读一遍**。

## 一、故障现象

内网部署（多条路由全部指向同一个 new-api 聚合网关，同 host、多 key），持续出现：

```
request failed after exhausting upstream candidates
upstream_status=502 downstream_status=503 failure_class=transient_server
route_action=exhausted same_route_retry=true cooldown_seconds=28
routing_round=1 physical_attempt_count=6 half_open_busy_count=0
error_category=upstream_routes_exhausted
```

模型主要是 GLM5.1、deepseek-v4-flash-0731。同时会时不时出现 400「上游拒绝请求」。

## 二、根因（已确认，无推断缺口）

聚合网关在 502 响应里带了 `Retry-After: 28`。本网关把这个 header 当成「路由摘除时长」
无条件写进健康注册表，而请求内轮间等待预算 `retry_max_wait` 恰好也是 30s。
**28s 冷却 > 30s − 已耗预算 ⇒ 重试策略必然返回 `GiveUpReason::WaitBudget` ⇒ 一轮轮间等待都不做就耗尽。**

证据链（每跳都已核对）：

1. `upstream_feedback.rs:718` — `parse_retry_after(input.headers)` 在 `classify_upstream_response` 顶部，**与失败分类无关**，502 也会解析
2. `upstream_feedback.rs:750-757` — 该值无 class 过滤地写入 `ClassifiedUpstreamFailure.retry_after`
3. `errors.rs:874-880` — 成为 `GatewayError::Classified.retry_after`
4. `gateway.rs:559` — `clamp_upstream_retry_after(error.retry_after(), retry_after_cap)`，实现见 `gateway.rs:653-655` 是 `.min(cap)`；cap 默认 30s，**28 < 30 原样通过**
5. `gateway.rs:663` — 同一个值进入 `route_health_outcome`
6. `route_health.rs:1353-1358` — `(_, Some(explicit)) => explicit.max(local)`，**上游值只能抬高、永不压低**冷却
7. `route_attempts.rs:847-853` — 成为 `TerminalFailure::Temporary{retry_after}`
8. `route_retry.rs:349-358` — `sleep_for(28s+jitter) > remaining(30s−waited)` ⇒ `GiveUpReason::WaitBudget`
9. `gateway.rs:7187` — 同路由重试的 backoff 也计入 `waited`，进一步压缩 remaining

**排除性证据（不要怀疑是本地退避升级）：** `record_failure_with_status` 全仓库只有一个调用点
（`gateway.rs:560`），本地 `jittered_backoff` 算出的冷却值从不写回 attempt ledger；
而 `cooldown_seconds` 取自 ledger（`gateway.rs:8054`）。若上游没发 header，
`route_attempts.rs:852` 的 `.unwrap_or(Duration::from_secs(1))` 会让日志显示 `cooldown_seconds=1`。
**所以 28 只能是上游 header。**

## 三、按此顺序实施

### 第 1 优先：T1.2 — 上游 `Retry-After` 与本地冷却解耦（根治项）

这是唯一「必要且充分」修掉本故障的改动。

- 新增 runtime setting `upstream_retry_after_cooldown_cap_seconds`，默认 **5**，范围 `1..=300`
  （`src/state/types.rs` 加常量 + 字段 + serde default；`src/state/runtime_settings.rs` 加字段、
  `SETTING_KEYS`(`:49` 附近)、`from_config`(`:284`)、`apply_to_config`(`:369`)、`validate`(`:511` 附近)；
  `src/main.rs:92` 附近加 env 读取）
- `src/server/gateway.rs:663` 的 `route_health_outcome`：**用于路由健康的** `retry_after` 改用新 cap 截断
- `src/server/gateway.rs:559`（写 ledger 的那处）同样改用新 cap，
  这样日志里的 `cooldown_seconds` 才反映真实冷却
- **保留** `upstream_retry_after_cap_seconds` 的原语义：只管**回给下游客户端**的
  `Retry-After` header（`errors.rs:1159`）与终态消息文案，默认仍 30
- `RouteFailureClass::ConcurrencySaturated` 分支（`route_health.rs:1354`）**必须豁免**新 cap
  —— 并发饱和上游给的 `Retry-After` 是真实槽位信息，削了会造成无效探测风暴
- 在 `route_health.rs:1353` 上方补注释，写明量纲区别：
  上游 `Retry-After` 是「客户端多久后再试」，不是「网关摘除路由多久」

### 第 2 优先：T1.1 + T1.3 — 防复发

**T1.1 冷却上界不变量**

```
ceiling = max(upstream_retry_after_cooldown_cap_seconds,
              transient_cooldown_base << (transient_cooldown_max_step - 1))
          .min(upstream_transient_route_cooldown_max_seconds)
要求：ceiling * 1000 < upstream_route_exhaustion_retry_max_wait_ms
```

- `runtime_settings.rs` 的 `validate` 加**跨字段**校验，不满足则拒绝保存，
  中文错误消息里必须写出两个具体数字
- `src/main.rs:248` 附近启动时同样校验：不满足则 `tracing::error!` 醒目告警
  **并自动把 `retry_max_wait_ms` 抬到 `ceiling * 1500`**，记 `auto_corrected=true`。
  **不要 panic**，内网可用性优先
- Admin 设置页对这两组字段做联动提示

**T1.3 非半开失败的 step 上限**

- 新增 runtime setting `upstream_transient_route_cooldown_max_step`，默认 **3**，范围 `1..=8`
- `route_health.rs:1852-1864` 的 `else` 分支（非半开）加 `.min(max_step)`；
  与半开分支已有的 `ROUTE_HALF_OPEN_FAILURE_STEP_CAP`(=5, `:1826`) 取更小者
- `failure_step` 目前是自由函数（`:1828`），需把 max_step 透传进去
- **本地 backend 与 Redis backend（`src/state/redis_runtime.rs`）行为必须一致**

### 第 3 优先：T0 — 可观测性（验收依据）

- **T0.1** `gateway.rs:8068-8091` 的 `tracing::error!` 增补字段：
  `give_up_reason`、`waited_ms`、`retry_max_wait_ms`、`retry_max_rounds`、`route_count`、
  `cooled_candidate_count`、`live_recovery_seconds`、`last_resort_probe_attempted`、
  `upstream_error_codes`、`distinct_upstream_hosts`。
  这些值多数已在 `errors.rs:176-230` 的 details map 里算过，复用即可。
  `retry_max_wait_ms` 与 `cooldown_seconds` 并排出现是关键设计——运维一眼能看到量纲矛盾
- **T0.2** 删掉 `gateway.rs:8081` 硬编码的 `remaining_candidates = 0`，
  改为真实可用候选数；若取值成本高就**直接删字段**，不要留恒 0 的误导项
- **T0.3** `continuation_candidate_count` 更名 `candidate_pass_count`
  （它是 `(能力档位 × 协议)` 通道数，不是路由数，见 `gateway.rs:5670-5713`），
  details map 里旧 key 保留一版并标 deprecated；新增真实的 `continuation_route_count`；
  `gateway.rs:5719` 的 `sole_contract_candidate` 判据也改用真实路由数
- **T0.4** `route_health.rs:1345-1360` 写 `cooldown_until` 时补日志：
  `route_id/class/step/step_suppressed/local_cooldown_ms/upstream_retry_after_ms/`
  `cooldown_source("local"|"upstream_retry_after")/effective_cooldown_ms/upstream_host`

### 第 4 优先：T1.4 + T2 — 让自愈在单聚合网关下可达

- **T1.4** 新增 `upstream_shared_host_failure_domain_enabled`（默认 true）：
  复用已有的 `upstream_host()`（`gateway.rs:797`），同 host 有 ≥2 条候选时，
  `TransientServer`/`EdgeProxyError` 族**不升级 step**、冷却走
  `EDGE_PROXY_ROUTE_BASE(3s)`/`EDGE_PROXY_ROUTE_MAX(15s)` 曲线（`route_health.rs:32-33`）。
  `Credentials`/key 配额类**仍走 per-key 冷却**，不要并进故障域
- **T2.1** `gateway.rs:7833-7838` 的 last-resort 探测 arm 条件，把
  `round_ledger.attempt_count() == 0` 改成允许「本轮打过但全部瞬态失败且已无可用候选」
  （需在 `route_attempts.rs` 新增 `is_all_transient_family_failures()` 与
  `available_candidate_count()`）。保持「每请求至多 1 次探测」不变
- **T2.2** 新增 `upstream_common_mode_same_host_transient_enabled`（默认 true）：
  `gateway.rs:7632-7640` 的共模 streak 判据放开同 host **仅对 transient 族**；
  `RequestRejected` 的请求形状断路器**保持 different-host 语义不变**（那是刻意设计，不要回退）
- **T2.3** `route_retry.rs:291-330` 与 `:349-358`：当 `sleep_for > remaining`
  且 `remaining >= 1s` 时，不要直接放弃，改为**等 `remaining` 后作为半开探测再打一次**，
  标记 `alignment_truncated=true`

### 第 5 优先：T3 — 400 自愈（与耗尽耦合）

- **T3.1** `capability_probe.rs:602-615` 的 `indicates_field_error` 补中文触发词：
  `参数非法`/`参数错误`/`参数有误`/`不支持`/`不支持该参数`/`无效的参数`/`无效参数`/
  `缺少必需参数`/`缺少参数`/`非法参数`/`未知字段`/`未知参数`；
  `upstream_feedback.rs:512-525` 的 `message_is_request_rejected` 补同一组
  （对齐 `:492-509` 的 `message_is_rate_limited` —— 那里**已经有**完整中文词表，
  唯独 request_rejected 是纯英文，这是覆盖不对称的直接反证）。
  **只加明确指向参数/字段的短语，不要加 `错误`/`失败`/`异常` 这类泛词**（会把真瞬态误判成不冷却）。
  `error_lower` 是 `to_ascii_lowercase()`，中文不受影响，**不要**改成 unicode lowercase
- **T3.2** `dialect_retry.rs:12-25` 的 `correction_for_response`：
  `/error/param` 缺失时回退到从 `error.message` 提字段名（复用 `dialect_field_error_hint`）；
  code 白名单加 `invalid_request_error`/`invalid_parameter_error`/`unsupported_value`/`invalid_value`；
  接受纯数字码（GLM 的 `1210` 族）但**必须联合判据**「数字码 + message 里有字段名」，不可单靠数字码放行。
  保留「仅 400 / 仅 `response_started == false` / body ≤64KB」三个护栏
- **T3.3** `capability_probe.rs:618-632` 字段表补
  `response_format`/`thinking`/`top_p`/`frequency_penalty`/`presence_penalty`/`seed`/`store`/`metadata`
  （**顺序敏感**：更长更具体的放前面，参照现有 `top_logprobs` 在 `logprobs` 之前）；
  `is_safe_dialect_strip_field`(`:641-655`) 只增补
  `response_format`/`seed`/`store`/`metadata`/`top_p`/`frequency_penalty`/`presence_penalty`，
  **`thinking` 不可加入**（承载推理开关语义，走 T3.4 preset 修正）。每个新增字段注释写明为何移除安全
- **T3.4** `UpstreamConfig` 新增 `model_dialect_presets: BTreeMap<String,String>`（`types.rs:576` 旁），
  保留 `dialect_preset` 作兜底。解析优先级：
  **已验证探测档案（`DialectProfileKey` 含 `runtime_model_slug`）> per-model preset > per-upstream preset > baseline**，
  落在 `src/capabilities/resolver.rs:53` 的 `dialect_preset` 入参处。
  持久化用 Postgres JSONB（参照 `src/state/postgres.rs:1862` 的 `ADD COLUMN IF NOT EXISTS`）
  + Admin API(`src/server/admin.rs:1089/1558`) + 前端。
  匹配语法**复用** `2026-08-13-per-upstream-model-mappings.md` 的既有约定，不要发明第二套
- **T3.5** `capabilities/types.rs:672` 的 `compile_dialect_preset` 里，
  给 `glm`/`deepseek` 分支补已知不支持字段到 `omit_sampling_fields`

### 第 6：T4 — 文档

重写 `DEPLOYMENT.md:185-215` 的 Intranet 小节为自洽参数表（见方案 §T4.1 的表），
并新增「`upstream_routes_exhausted` 三步定位法」runbook（方案 §T4.2）。

## 四、测试要求（必须全部通过）

方案文档 §4 有完整清单，其中**三条是根因守卫，必须写**：

1. `route_attempts.rs` — 上游无 `Retry-After` 的 502 序列 ⇒
   `TerminalFailure::Temporary.retry_after == 1s`，证明本地 backoff 值不漏进 ledger
2. `upstream_feedback.rs` — 502 + `Retry-After: 28` ⇒
   `retry_after == Some(28s)` 且 `class == TransientServer`，锁住 `parse_retry_after` 的 class 无关性
3. `tests/gateway/route_exhaustion_budget_invariant.rs`（新增）— **端到端复现用户日志**：
   3 路由 × 2 key = 6 候选全部 `502 + Retry-After: 28 + JSON body`
   - 修复前断言 `routing_round==1 && physical_attempt_count==6 && cooldown_seconds==28 && give_up_reason==wait_budget`
   - 修复后断言 `cooldown_seconds<=5 && routing_round>=2`，上游第 2 轮恢复时最终 200

另需：`ConcurrencySaturated` 的 `Retry-After` **不**被新 cap 削减的回归保护；
`502 + nginx HTML` 仍判 `EdgeProxyError` 的回归保护；
HTTP 500 + 中文 `参数非法` ⇒ `RequestRejected`（**不是** `TransientServer`）；
`presence_penalty` 不被子串误匹配；本地与 Redis backend 的 step 封顶一致。

## 五、约束

- **所有 shell 命令加 `rtk` 前缀**（含 `&&` 链中的每一段），项目 CLAUDE.md 硬性要求
- 验证：`rtk cargo build` → `rtk cargo clippy --all-targets -- -D warnings` →
  `rtk cargo test --lib` → `rtk cargo test --test gateway` → `rtk cargo test`
- 每个行为变更都要带 runtime setting 开关，可单独关闭回退
- 现有测试必须保持绿，特别是 `tests/upstream_retry_after_cap.rs`、
  `tests/gateway/responses/continuation_escape.rs`、`tests/gateway/chat/half_open_busy_ledger.rs`
- 完成后在方案文档 §6 的回填表里填 commit hash 并把 ⬜ 改 ✅
- 分阶段提交：T1.2 单独一个 commit（便于内网优先热修），其余按 T 号分组
