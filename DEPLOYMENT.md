# Deployment Runbook

`chat-responses-codex` supports a local coordination mode with one authoritative
gateway instance and an optional Redis coordination mode for replicas. Both
modes use PostgreSQL 15 as the durable source of truth.

## Operating Model

- With `REDIS_ENABLED=false`, run one authoritative gateway instance per PostgreSQL database.
- With `REDIS_ENABLED=true`, replicas share admission windows, exact leases, route cooldowns, and half-open ownership through Redis.
- Keep PostgreSQL on a private network or managed service. Do not publish it directly to the public internet.
- Mount or provision durable storage for PostgreSQL so keys, upstreams, downstreams, and usage logs survive restarts.
- Place a reverse proxy or load balancer in front if the service is exposed outside a trusted network.
- Redis does not replace PostgreSQL. Every coordinated replica still connects to the same durable database.
- Give each deployment sharing Redis a unique `REDIS_KEY_PREFIX`; never reuse a prefix across environments.
- `STATE_PATH` remains only for the file-backed compatibility mode when `DATABASE_URL` is unset.

## Required Environment

The checked-in [.env.example](.env.example) contains only process bootstrap,
credentials, and infrastructure settings. Review these values before a
production-like run:

- `BIND_ADDR=0.0.0.0:3001`
- `STATE_PATH=/data/state.json` for file-backed compatibility mode only
- `DATABASE_URL=postgres://chat_responses_codex@postgres/chat_responses_codex`
- `POSTGRES_PASSWORD=<strong-secret>`
- `POSTGRES_POOL_MAX_SIZE=16`
- `LOG_PATH=/logs/chat-responses-codex.log`
- `RUST_LOG=info`
- `TZ=Asia/Shanghai`
- `ADMIN_USERNAME=admin`
- `ADMIN_PASSWORD=<strong-secret>`
- `JWT_SECRET=<strong-secret-at-least-32-characters>`
- `REDIS_ENABLED=false`
- `REDIS_URL=redis://redis:6379`
- `REDIS_KEY_PREFIX=chat2responses`
- `UPSTREAM_CA_CERT_PATH=`
- `CAPABILITY_POLICY_BOOTSTRAP_ON_ZERO=true`

Saved values from Admin > Settings override legacy behavior environment
variables. Existing variables are used only until the first settings save.
Bootstrap connections and credentials remain environment-only.

Capability bootstrap only replaces a stored policy at revision 0. Set
`CAPABILITY_POLICY_BOOTSTRAP_ON_ZERO=false` to keep revision zero and opt out.

The first save persists the complete settings document. Later starts use that
document instead of legacy behavior variables, and the gateway never rewrites
the operator's `.env`. The checked-in Compose file no longer passes through behavior environment
variables: it wires bootstrap and credentials only, and all behavior settings
are maintained in `Admin > Settings`.

Use `/admin/settings` for application identity, discovery, capability probes,
routing, concurrency, HTTP, and log retention. Each field is marked either
immediate or restart-required. Immediate changes apply only to operations that
start after the save; restart-required changes remain pending until the gateway
process restarts. Database and Redis connections, credentials, log bootstrap,
and internal CA trust remain environment-only and never appear in this API.

Recommended upgrade sequence:

1. Preserve the existing operator `.env` and upgrade the gateway.
2. Open `Admin > Settings` and review the effective values inherited at startup.
3. Save once to establish the persisted document.
4. Restart if the page reports pending restart-required fields.
5. Remove legacy behavior variables from the operator `.env` when convenient.

For HTTPS upstreams signed by an internal CA, place the CA certificates in the
repository-local `certs/` directory and set `UPSTREAM_CA_CERT_PATH=/certs`.
The path may also point to one PEM bundle file. Directory mode loads regular
`.crt` and `.pem` files in file-name order, and every file may contain multiple
PEM certificates. Public WebPKI roots remain enabled; configured internal roots
are additive. The Compose mount is read-only, environment certificate files are
ignored by Git, and the gateway must be restarted after certificate changes.
Do not place server private keys in `certs/` and do not disable TLS verification.

## Runtime Settings Operations

The settings page groups all managed behavior under General, Discovery,
Routing, Concurrency, HTTP, and Logs. It validates relationships before saving;
for example, stream keepalive must remain below the idle timeout, the transient
cooldown base cannot exceed its maximum, and probe delays are normalized.

The capability probe queue capacity limits pending atomic submission batches,
not the number of routes inside a batch. Accepted batches expand immediately
into the route-key-deduplicating scheduler. Automatic capability probes are
disabled by default because they send real inference requests and consume
model tokens; manual probes and “真实验证并应用” still consume tokens when an
administrator explicitly runs them.

Probe timeouts and concurrency are managed settings: ordinary probe cases
use a 20-second request timeout, reasoning-control and reasoning-triggered
cases use a 90-second timeout, and probe batches default to 4 concurrent
routes. Saved values from Admin > Settings override the startup fallbacks.
Reasoning-only batches additionally serialize work per upstream and space
cases to reduce internal gateway RPM bursts.

Automatic upstream model discovery is disabled by default. Manual model
discovery remains available when automatic discovery is disabled and persists
only the models selected when the upstream is saved. Set the background
model-key synchronization interval to 0 to disable background model-key
synchronization.

Real upstream 429 responses cool the exact route and switch immediately to
another eligible candidate. An explicit provider `Retry-After` always takes
precedence. Concurrency-specific 429 responses without that header use the
configured probe-delay sequence, repeat its final delay, and allow only one
half-open probe per exact route at a time.

Generic Transport/5xx failures may retry the same exact route once. Turning
that option off does not disable the initial routing round: Key and upstream
fallback remains available. The exact-route cooldown grows exponentially with
deterministic jitter from the configured base to the configured maximum.

The slow-first-output hedge delay controls the first extra attempt, the hedge
interval spaces later attempts, and the maximum extra-attempt count bounds the
fan-out. A high-utilization internal profile can use a 2-second delay and
interval with two extra attempts. This permits at most three admitted attempts
for one logical stream, and every attempt still requires normal upstream
concurrency and quota admission.

For repeated Transport/5xx failures amplified by client retries, disable the
fixed same-route retry and use a 3-second base with a 60-second cooldown cap.
Codex owns SSE interruption retries under this profile. Keep
`stream_max_retries = 2` in the generated Codex configuration; the gateway's
initial Key/upstream fallback remains independent of that client retry budget.

Setting the route-exhaustion wait budget to `0` means zero disables waiting,
and total rounds include the initial round. The gateway preserves the full
`Retry-After`; configured priority cannot make an unhealthy route eligible;
output or tool calls are never replayed after delivery. To roll back the
multi-round behavior, disable route-exhaustion retry in `Admin > Settings` and
save; restore the conservative hedge timings there if needed.

## Multi-Key Route Resilience And Upgrade

Each key under an upstream account has a separate persisted model mapping. A
successful discovery returning no models is an authoritative empty mapping: the
key supports no models and does not inherit the upstream-level list. After an
upgrade, deployments with empty persisted `supported_models` must complete one
successful explicit discovery, or one complete background legacy discovery,
before `/v1/models` advertises those models. `/v1/models` reads only the
persisted model catalog.

Runtime health is deliberately separate from capability persistence. A generic
upstream 5xx retries the same exact route once before another route is selected.
An upstream 429 stores the full `Retry-After` on the exact route and switches to
another eligible route. A new round starts only after temporary all-route
exhaustion and only when exact recovery fits the remaining request wait budget.
If the budget cannot fit another round, the terminal `Retry-After` comes from
the health registry's live earliest route recovery. Pure upstream rate-limit,
concurrency, or key-quota exhaustion returns 429; a mix containing 5xx,
transport, or generic capacity failure remains 503. The safe message includes
failure causes, route counts, and time already spent in gateway retries.
Automatic replay before usable output reuses the same idempotency identifier,
but remains at-least-once when the provider does not honor an idempotency header,
so retries can duplicate inference or provider-side storage.

In local mode, runtime route health resets on restart and the next request
re-evaluates the route. In Redis mode, cooldown and half-open ownership are
shared across replicas and survive individual gateway restarts for their
bounded Redis lifetime. Runtime coordination does not change the persisted model catalog
and is never consulted for `/v1/models`.

When Redis coordination is enabled, startup is fail fast: an invalid prefix,
missing URL, or unavailable Redis prevents the gateway from starting. Runtime
operations fail closed: health checks and requests that depend on coordination
return 503 instead of silently using local counters. Error and structured logs
do not include Redis URLs, key names, admin credentials, or downstream keys.

Stable client outcomes:

| HTTP / code | Operator action |
|-------------|-----------------|
| 429 `upstream_routes_exhausted` | Every route is upstream rate-limited, concurrency-saturated, or key-quota exhausted; retry using `Retry-After` |
| 503 `upstream_routes_exhausted` | Temporary exhaustion includes a 5xx, transport, or generic capacity failure; retry using `Retry-After` |
| 502 `upstream_credentials_exhausted` | Every eligible key has a credential, balance, or billing failure |
| 502 `upstream_model_unsupported` | Every attempted route rejected the requested model |
| 400 `capability_not_supported` | No route can preserve an explicitly required feature |
| 502 `upstream_protocol_unsupported` | No route supports the requested endpoint or protocol |
| 400 `upstream_request_rejected` / 502 `upstream_request_shape_rejected` | Multiple routes rejected the same request shape; stop replaying and fix the request or the upstream policy instead of retrying |
| 502 `upstream_transient_pool_failure` | Distinct upstream hosts failed with identical transient errors even after one delayed replay round (likely a shared gateway / egress outage); retry using `Retry-After` |

### Intranet / Aggregated Gateway Deployment

When several "different" routes share the same physical infrastructure (one
one-api / new-api aggregated gateway, one egress proxy, or one upstream host
with multiple keys), the common-mode breaker must not treat a transient outage
of that shared hop as a request-shape problem. The breaker counts only
identical `(failure class, HTTP status)` failures across routes on **different
hosts**; a repeated failure on the same route or the same host restarts the
streak.

Relevant settings (see `Runtime Settings Operations`):

| Setting | Default | Meaning |
|---------|---------|---------|
| `upstream_common_mode_breaker_threshold` | 2 | Request-shape breaker (`RequestRejected` only). Reached → stop replaying, return 400/502 immediately. `0` disables. |
| `upstream_common_mode_transient_threshold` | 4 | Transient breaker (`TransientServer` / `EdgeProxyError`). Reached → one delayed replay round (≤500ms) before returning 502 `upstream_transient_pool_failure` with `Retry-After`. `0` disables this class. Validated ≤ 64. |
| `upstream_transient_same_route_retry_enabled` | true | Retry the same route once (200–500ms backoff, honoring upstream `Retry-After` up to 2s) on transient 502/503/504 before entering failover. Only applies before any byte was sent downstream. |
| `upstream_same_route_retry_enabled` | true | Master switch for the same-route retry above; both switches must be on for the retry to fire. |
| `upstream_transient_route_cooldown_base_seconds` | 10 | Base cooling time for transient server failures; escalates exponentially with the failure streak. Keep low (2–3) for an aggregated gateway so a short upstream blip recovers fast. |
| `upstream_transient_route_cooldown_max_seconds` | 300 | Cap on the escalated transient cooldown. Bound it (60 or less) for an aggregated gateway: with `base = 2` and `max = 60`, a real outage settles at roughly 1–3 rounds of escalation instead of minutes. |
| `upstream_route_exhaustion_retry_max_rounds` | 3 | Routing-round cap for the temporary route-exhaustion retry path. With budget alignment on (default), keep 3: the cap bounds blind retries while the time budget governs evidence-backed aligned waits. If you turn `upstream_route_exhaustion_budget_alignment_enabled` off, raise this to 6 so round count does not bite before the time budget. |
| `upstream_route_exhaustion_budget_alignment_enabled` | true | When the round cap is hit but a live transient recovery fits the remaining wait budget, grant one final aligned wait before giving up (and never for pure 429-family exhaustions, which are always returned to the client). |
| `upstream_transient_last_resort_probe_enabled` | true | When a routing round made zero physical attempts because every candidate is cooling, arm the earliest-recovering route as an early half-open probe: the next request itself tests it (single-flight + ≥1s per-route interval). Turns a cooled-down pool into self-healing instead of waiting out the cooldown clock. |
| `upstream_route_half_open_exclusive_window_ms` | 3000 | Route re-probe exclusivity window: one request probes a recovering route while the pool reports it "half-open busy"; after the window elapses concurrent requests are admitted again. `0` disables exclusivity (everyone may race into the probe). Harmless leftover probe leases are bounded by `upstream_route_health_half_open_ttl_seconds`. |
| `upstream_route_half_open_busy_max_rounds` | 10 | When the whole pool is only half-open-busy, the request may poll on a dedicated busy-round budget that does **not** consume ordinary `upstream_route_exhaustion_retry_max_rounds`; exhaustion reports `give_up_reason = half_open_busy_cap`. |
| `upstream_retry_after_cap_seconds` | 30 | Ceiling for upstream-provided `Retry-After` (429/503): larger values are clamped for cooldowns, terminal headers and admin snapshots. Raise to 3600 to trust upstream values verbatim. |
| `upstream_credentials_first_strike_seconds` | 60 | First 401/403 for an API key cools the key only this long; consecutive strikes escalate onto the 15min→1h exponential curve. |
| `upstream_error_body_excerpt_enabled` | false | Append a bounded, sanitized excerpt of the upstream error body to client error messages (`body=\"…\"` in the terminal summary, `; upstream_body=\"…\"` on single-attempt errors). The excerpt passes a secret-shaped redactor (`sk-…` keys, `Bearer` tokens, JSON secret pairs) before it is exposed. **Only enable for intranet self-owned upstreams where you operate both sides; keep off for public / multi-tenant deployments.** |
| `upstream_error_body_excerpt_max_chars` | 200 | Maximum excerpt length (50–2000); longer bodies are truncated with `…`. Only read when the excerpt switch is on. |
| `upstream_continuation_pin_escape_enabled` | true | When the previously-successful continuation route is unavailable, allow the gateway to purge supplier-bound artifacts from the session history and re-run the full candidate pool, re-pinning the session to a working route. Off = the session can only wait for the original route to recover. |
| `upstream_local_lease_ttl_seconds` | 3600 | TTL for an upstream concurrency lease that was never released. Both local and Redis backends use it; running streams renew their lease. Keep it above the expected heartbeat outage window and long enough for the request watchdog to distinguish a live stream from a leak. |
| `upstream_account_queue_enabled` | true | When a round fails purely because the local pre-dispatch concurrency gate is full (no upstream attempt was made), queue the request behind the free slot (C3) instead of rejecting + retrying. Off = immediate rejection (which then uses the C4 fast-fail / legacy code). |
| `upstream_account_queue_max_depth` | 16 | Bound on how many requests may wait on one (upstream, key) local-concurrency queue at a time; overflow fast-fails. |
| `upstream_account_queue_max_wait_ms` | 10000 | Deadline for a queued local-concurrency wait; expiry fast-fails with the C4.2 error code. |
| `upstream_local_gate_max_wait_ms` | 3000 | Bounds the C4.1 fast-fail scenario: a round served entirely by local-gate rejections gives up immediately instead of burning the 30s ConcurrencySaturated budget. It realises as an immediate rejection, so a lowered value also lowers the worst-case client wait for a fully-saturated single-key upstream. |
| `upstream_local_gate_fast_fail_enabled` | true | Master switch for C4.1. On: a round whose every candidate was rejected by the *local gate* (zero physical upstream attempts) fast-fails. Off: old behavior (falls into the ConcurrencySaturated retry budget). |
| `upstream_local_gate_distinct_error_code_enabled` | true | C4.2: terminal code `gateway_concurrency_saturated` (HTTP still 429) so a gateway-gate fact is never misread as upstream rate limiting. Off: legacy `upstream_routes_exhausted` aggregation for local-gate rejections. |

### Redis 后端与本地后端的能力差异

生产环境使用 PostgreSQL + Redis 时，Redis 是运行时准入、租约和路由健康的共享协调器；PostgreSQL 仍保存配置和 usage log。两种后端的行为边界如下：

| 能力 / 设置 | 本地后端 | Redis 后端 | 运维含义 |
| --- | --- | --- | --- |
| 上游租约 TTL | `upstream_local_lease_ttl_seconds` | 同一设置 | 修改该设置对生产 Redis 部署生效。**它是泄漏租约滞留时间的唯一决定项**——要压缩滞留时间就调它，不要调 `upstream_stream_max_duration_seconds`（后者只决定单条流最长存活，见下方分工） |
| 运行中租约续约 | 进程内心跳 | Redis 心跳续约 | 长流只要网关仍在运行就不会因 TTL 被误回收 |
| 陈旧租约提前回收 | 支持 | 支持，按 `upstream_lease_stale_after_ms` | 泄漏租约会早于 TTL 被回收，并计入 `stale_reclaimed_total` |
| 租约持有 p50/p95 | 支持 | 不采样，返回 `null` | 管理界面显式渲染「本后端不支持」，不会把缺失当成 0 |
| 排队深度 | 进程内真实值 | 观测不到，返回 0 并标记 `queue_depth_supported=false` | 管理界面显式渲染「排队：本后端不支持」 |
| 本地闸门拒绝累计 | 进程内计数 | Redis 计数器 | `capacity_reject_total` 可用于定位网关侧容量拒绝 |
| 免冷却放行累计 | 进程内计数 | Redis 计数器（同一计数） | `route_cooldown_skipped_total` 两种后端都是真实值 |
| 多副本共享准入与路由健康 | 不共享，单实例 | 通过同一 Redis 和 `REDIS_KEY_PREFIX` 共享 | 每个部署使用独立 prefix；Redis 不可用时 fail closed |

应用行为设置统一在 `Admin > Settings` 保存。尤其是 `upstream_local_lease_ttl_seconds`、`upstream_lease_stale_after_ms` 和心跳相关设置，修改后应结合上游运行时快照观察 `in_flight`、陈旧回收计数和租约年龄；不要仅依据单一路径的缺失统计判断“没有发生”。

**两个时长设置的分工（G7）**：

| 设置 | 职责 | 消费点 |
| --- | --- | --- |
| `upstream_stream_max_duration_seconds`（默认 86400） | 单条上游流的最长存活，超时判定 `stream_max_duration`；**与租约回收无关** | 流读取循环（`src/server/gateway.rs` 的 stream max-duration 判定） |
| `upstream_local_lease_ttl_seconds`（默认 3600） | 上游并发租约的 TTL：既是运行中续约的租期，也是**泄漏租约的滞留上限** | Redis 租约 ZSET 分值 / 本地租约表 |

> 历史遗留纠正：F1.1 之前 `upstream_stream_max_duration_seconds` 曾兼任 Redis 租约时长，旧文档给过“调小它可以压缩泄漏租约滞留时间”的缓解建议——**该建议已作废**。压缩滞留时间应调小 `upstream_local_lease_ttl_seconds`（同时要大于预期的续约中断窗口，见上方默认值行）。

Recommended values for a single aggregated gateway deployment:

The rows below are **self-consistent by construction**: the T1.1 cooldown-ceiling
invariant is what prevents a route cooldown from ever eating the whole retry
budget (`upstream_route_exhaustion_retry_max_wait_ms`).

> **Cooldown ceiling invariant (T1.1).** Let `base = upstream_transient_route_cooldown_base_seconds`,
> `step_max = upstream_transient_route_cooldown_max_step`, and
> `wait_max_ms = upstream_route_exhaustion_retry_max_wait_ms`. The worst-case
> effective cooldown is
> `ceiling = max(upstream_retry_after_cooldown_cap_seconds, base << (step_max - 1))`
> capped at `upstream_transient_route_cooldown_max_seconds`, and the invariant
> requires `ceiling * 1000 < wait_max_ms`. If you change any of these fields,
> Admin > Settings rejects an invalid combination, and the gateway logs a loud
> startup warning (auto-raising `wait_max_ms` to `ceiling * 1500`) if a config
> still violates it.

| Setting | Public default | Intranet aggregated-gateway value | Why |
|---------|---------------|-----------------------------------|-----|
| `upstream_transient_route_cooldown_base_seconds` | 10 | **2** | A shared-hop blip lasts 2–3s; a short base lets the route come back fast. |
| `upstream_transient_route_cooldown_max_seconds` | 300 | **15** | Aligns the ceiling with `EDGE_PROXY_ROUTE_MAX`; bounded escalation instead of minutes. |
| `upstream_transient_route_cooldown_max_step` | 3 | **3** | `2 << 2 = 8s`, ×1.2 escalation ≈ 9.6s worst case — far inside the 30s budget. |
| `upstream_retry_after_cooldown_cap_seconds` | 5 | **5** | Upstream-provided `Retry-After` must never dominate local cooldown; 28s headers collapse to 5s. |
| `upstream_route_exhaustion_retry_max_wait_ms` | 30_000 | **30_000** | `ceiling = max(5, 8) = 8s`, `8_000 < 30_000` ⇒ invariant satisfied. |
| `upstream_route_exhaustion_retry_max_rounds` | 3 | **4** | A single-host pool benefits from more in-request probing rounds before giving up. |
| `upstream_shared_host_failure_domain_enabled` | true | **true** | Same-host candidates share one failure domain: they do not upgrade each other's cooldown step. |
| `upstream_common_mode_same_host_transient_enabled` | true | **true** | Same-host common-mode transient failures get the delayed-replay round instead of an immediate hard failure. |
| `upstream_error_body_excerpt_enabled` | false | **true** | You operate both sides of the intranet upstream; error excerpts are required for diagnosis. |

Keep these as-is for the aggregated-gateway shape:

- `upstream_common_mode_transient_threshold = 0` (or `>= 4` when you have ≥4
  genuinely distinct upstream hosts and want pool-outage detection). With one
  aggregated gateway, transient 502s always share the single hop; `0` keeps the
  code path simple and relies on per-route failover + the temporary recovery
  rounds bounded by `upstream_route_exhaustion_retry_max_rounds`.
- Keep `upstream_common_mode_breaker_threshold` at its default; `RequestRejected`
  (400 semantics) repetition is still a genuine request-shape signal, and the
  request-shape breaker deliberately keeps its **different-host** semantics.
- Keep the half-open governance defaults:
  `upstream_route_half_open_exclusive_window_ms = 3000` (a recovering route is
  monopolized by one probe request for at most 3s, then others may race in),
  `upstream_route_half_open_busy_max_rounds = 10`,
  `upstream_retry_after_cap_seconds = 30` (the header returned **to the client**
  — not the cooldown — stays capped at 30 so a quota-style 429 does not tell the
  client to wait an hour), and `upstream_credentials_first_strike_seconds = 60`.
- `upstream_route_exhaustion_budget_alignment_enabled = true` and
  `upstream_transient_last_resort_probe_enabled = true` (both defaults) keep the
  pool self-healing: an aligned wait rides out a live recovery, and a fully
  cooling pool arms the earliest route as an early half-open probe.

- **Concurrency capacity (the other half of `upstream_routes_exhausted`):**
  each upstream admits at most `max_concurrency` concurrent streams per
  (upstream, key) — default **32** for newly created upstreams (E4).  Existing
  deployments keep their persisted value; see the batch-adjustment note below.
  **Semantics changed (E4): `max_concurrency` is no longer a "master gate"
  that tries to mirror upstream capacity — it is a anti-snowball safety net.**
  The upstream itself is the authority on its own capacity: it answers
  honestly with 429 when saturated, and the client's retry loop converges
  (this is exactly what happens when the client talks to the upstream
  directly).  The gateway only enforces this net to keep a runaway client
  from pinning every slot for minutes.  Because the local gate is a net, its
  own rejection is cheap and never cools the route (E1) and its `Retry-After`
  is estimated from observed lease hold times, not TTL (E3).

  Batch-adjust `max_concurrency` for many upstreams at once:

      POST /api/admin/upstreams/batch-update
      { "ids": [101, 102, ...], "updates": { "max_concurrency": 32 } }

  The endpoint answers per-id success/failure and 400s with the rejected
  field names on any unknown field — it never silently ignores an update.

> **`ConcurrencySaturated` has its own retry budget — do not compare rounds (C4.3).**
> The ConcurrencySaturated path (429 family, including the gateway's own local
> gate) runs on `upstream_concurrency_recovery_max_rounds = 32` and
> `upstream_concurrency_recovery_max_wait_ms = 30_000`,
> completely independent of `upstream_route_exhaustion_retry_max_rounds = 3`.
> An error message that says "retried for 32.1s across 6 routing rounds" is
> *not* evidence the round cap was changed or exceeded — it used the 30s
> time budget of the concurrency path and is well below 32 rounds. Do not
> "fix" the round count; the numbers are not comparable.
> - Since C4, a request that never reaches the upstream and only ever hits the
>   gateway's own local gate (leaked/held slots, `max_concurrency` reached)
>   fast-fails with code `gateway_concurrency_saturated` (HTTP 429) instead of
>   spooling through that budget — bounded by
>   `upstream_local_gate_max_wait_ms`. Read its
>   `details.in_flight / max_concurrency / stale_lease_count / queue_depth` to
>   see whether live requests or stale leases hold the slots.
> - A route-health `ConcurrencySaturated` recovery (upstream actually returned
>   429, or a cooldown is in flight) is *not* a local-gate verdict and still
>   waits on the 32-round budget via the shared-probe recovery. Claude Code / Codex spawn parallel
  sub-tasks and hold long streams, so a small team can pin 4 slots for
  minutes. If `details.class_counts.concurrency_saturated` (or the
  `concurrency_saturated` share of 429s) stays high after the half-open /
  Retry-After fixes, raise the per-upstream `max_concurrency` to **16–64**
  for an internal aggregated gateway and raise
  `default_upstream_max_concurrency` for upstreams without an explicit
  value. `requests_per_minute` and the 5h quota only gate **hedge** attempts,
  not primary windows, so they are rarely the limiter here.

#### Why capacity-class failures never cool routes (E1/E2)

Route-health cooldown means: **"this path is unhealthy, move traffic away."**
Capacity-class failures do **not** carry any health information — the
upstream is explicitly saying *"I am healthy, I am just full right now."*
Writing that into the health registry is a semantic misuse.  Its concrete
cost on the intranet shape is global breakage: a `429` from the single
aggregated gateway (or the gateway's own local gate) cooled the *only* route
for minutes, so the client's correct retry loop was served
`upstream_routes_exhausted` without a single upstream request.

Therefore, since the admission semantics work:

- `RateLimited` / `KeyQuota` failures that are client-retryable, and the
  local gate's `ConcurrencySaturated`, are **capacity class**: they are
  observed (`last_failure_class`, counters, `last_retry_after_seconds`) but
  never write `cooldown_until` and never advance `consecutive_failures` —
  **including the key-level cooldown path** (`KEY_COOLDOWN_MAX` is 60
  minutes; a 429 escalating to that would be worse than today).
- `E2` adds a guard independent of the E1 switch: when a
  `(runtime_model_slug, protocol)` has exactly **one** available route, any
  capacity-class failure is unconditionally not cooled — with a single route
  the cooldown's "switch to another path" benefit is zero and its cost is a
  total outage.
- The health class is a **whitelist** of exactly these capacity classes;
  everything else — `TransientServer`, `EdgeProxyError`,
  `CapacityUnavailable`, `ModelUnsupported`, credential failures — keeps its
  exact previous cooldown and escalation behavior.  The upstream's own 429 is
  now the only authority, and backpressure on a misbehaving client belongs to
  the downstream key's `per_minute_limit`, not to route health.

Controls: `upstream_capacity_failure_cooldown_enabled` (default **false** —
setting it `true` restores the old cooldown-on-429 behavior as a rollback
path), plus the `E2` guard is not switchable.

#### How this differs from the `2026-08-27` plan (and why)

The 2026-08-27 concurrency plan stated "`max_concurrency = 4` is the correct
factory default — do not change it."  That conclusion rested on the
assumption that *more than 4 concurrent requests would knock the upstream
over*.  The on-site incident clarified the actual contract: the upstream
answers honestly with 429 when saturated and the client's retry loop
converges.  With that contract the gateway does not need to predict upstream
capacity — doing so with a stale, leased-slot count is strictly worse than
letting the upstream answer.  The correct default is therefore a
safety-net-scale number (32), and **the upstream is the authority on its own
capacity**.  Do not "fix" the default back to 4; see the batch-update note
above if a deployed fleet still carries the old persisted value.

#### Downstream per-model concurrency groups (C7)

The **downstream** gate (`try_reserve_downstream_concurrency`, per
`DownstreamConfig.max_concurrency`) is model-agnostic by default. When one
downstream key serves models with very different upstream capacities, a single
number is wrong for all of them, and worse, it head-of-line-blocks: a request
holds its downstream lease from before upstream routing starts until the whole
upstream retry/queue finishes, so a burst on the small-capacity model occupies
every slot and the big-capacity model cannot even enter the downstream gate.

`DownstreamConfig.model_concurrency_groups` splits the global budget per model:

```jsonc
"model_concurrency_groups": [
  { "name": "glm",      "match": ["glm-5.2", "glm-5.1"], "max_concurrency": 4  },
  { "name": "deepseek", "match": ["deepseek-*"],          "max_concurrency": 28 }
]
```

- `match` patterns support exact names and `*` wildcards (prefix / suffix /
  any-position); the list is **ordered, first match wins**. Matching runs on the
  alias-resolved `normalized_model`, honoring `model_case_insensitive_matching`.
- Models matching **no** group fall through to the global `max_concurrency` cap
  (no error, no rejection) — new models cannot silently bypass or be starved.
- Each group is checked against its own cap **and** the global cap (both must
  pass), so the global `max_concurrency` stays a downstream-wide backstop. The
  global cap also bounds the gateway process's own resources (SSE connections,
  tasks), a dimension group caps do not cover.
- Empty `model_concurrency_groups` behaves byte-for-byte like the legacy gate.
- Sum of group caps may legally exceed the global cap (later groups then rely on
  the global backstop); the admin API logs a warning, it is not an error.

**Site reference values** (group cap = number of upstream keys serving that
model × 4, matching the upstream `max_concurrency` hard ceiling):

| Group | Upstream keys | Group cap |
|-------|---------------|-----------|
| glm5.2 | 1 | **4** |
| deepseek | 7 | **28** |
| global `max_concurrency` | — | **32** |

No extra headroom is needed — overflow is absorbed by the C3
`upstream_account_queue_*` queue; that is exactly what C3 is for.

**Three operational rules:**

1. **Group caps are per downstream key, not gateway-wide.** With M>1 downstream
   keys each carrying `deepseek=28`, M×28 requests could cross the downstream
   gate for the same 28 upstream slots. Either split capacity per key (each
   key's cap = 28/M) or accept a deeper C3 queue (`upstream_account_queue_max_wait_ms`
   gets hit sooner). Single-key deployments (the site shape) avoid this.
2. **A key serving multiple models shares its 4 upstream slots across them —
   do not count it in two groups.** Group capacity = (keys serving *only* that
   model × 4) + (shared keys' apportioned share). If glm's key and deepseek's
   keys do not overlap, glm=4 and deepseek=28 are correct as-is; if they
   overlap, the sum of group caps must not exceed (non-duplicated key count × 4).
3. **`rate_limit_enabled = false` skips the whole downstream gate** (the
   gateway logs a warning when it is off), which disables *both* group caps and
   the global cap and slams everything into the upstream local gate. Default is
   `true`; do not turn it off "for speed".

Edit the groups per downstream key via the admin UI (Downstreams → 编辑 → 模型并发组)
or the update API. For bulk maintenance across many keys (the common shape —
several downstream keys share the same group set), use
`POST /api/admin/downstreams/batch-update` with
`{ "ids": [...], "updates": { ..., "model_concurrency_groups": [...] } }`
(whitelist-gated, per-id success/failure, mirrors the upstream batch-update).

### `upstream_routes_exhausted` 三步定位法 (runbook)

When a request fails with `503 upstream_routes_exhausted`, read the terminal
`tracing::error!` line and walk these three steps in order:

1. **看 `give_up_reason`** (which budget ran out first)
   - `wait_budget` ⇒ the cooldown ceiling invariant is violated: compare
     `cooldown_seconds` against `retry_max_wait_ms` side by side (they appear
     adjacent in the log by design). Fix the config, not the code.
   - `round_cap` ⇒ raise `upstream_route_exhaustion_retry_max_rounds`.
   - `no_recovery` ⇒ the health registry has no recovery information at all;
     check the Redis / local backend and whether failures are being recorded.
   - `half_open_busy_cap` ⇒ probe congestion: raise
     `upstream_route_half_open_exclusive_window_ms` or check upstream
     first-packet latency.
2. **看 `cooldown_source`** (why routes are cooling)
   - `upstream_retry_after` ⇒ an upstream `Retry-After` header is driving the
     cooldown; tune `upstream_retry_after_cooldown_cap_seconds` (default 5s —
     the header tells the *client* when to retry, not how long the gateway
     must quarantine a route).
   - `local` ⇒ look at `step` and escalate with
     `upstream_transient_route_cooldown_base_seconds` /
     `upstream_transient_route_cooldown_max_step`.
3. **看 `distinct_upstream_hosts`** (is the diversity real?)
   - `= 1` ⇒ every "different route" is physically the same aggregated
     gateway; confirm `upstream_shared_host_failure_domain_enabled` and
     `upstream_common_mode_same_host_transient_enabled` are both **true** (they
     are by default) so same-host candidates do not escalate each other.
   - If `= 1` stays true for a long time, route diversity is illusory: scale the
     real upstream (new-api / the egress proxy) instead of adding gateway
     routes — the gateway can only fail over to paths that actually exist.

Troubleshooting:

- Downstream sees `502 upstream_transient_pool_failure` with
  `details.common_mode = true`, `details.retried = true`: several distinct
  upstream hosts failed identically even after the replay round. Check the
  shared egress / aggregated gateway, not the request. `details` include
  `failed_route_count`, `distinct_hosts`, `streak`, `threshold`.
- Downstream sees `429 gateway_concurrency_saturated` (C4): the gateway's
  *own* local pre-dispatch gate rejected the request — the upstream was never
  called and is **not** rate limiting you. Read `details`:
  - `in_flight` / `max_concurrency` — how many of the (upstream, key) slots
    are held (`in_flight == max_concurrency` with nothing actually running ⇒ a
    leaked lease; it self-heals via `upstream_lease_stale_after_ms`).
  - `stale_lease_count` — leases whose heartbeat lapsed; a non-zero count
    means slots are held by dead owners that the stale sweep is about to free.
  - `queue_depth` — how many requests are queued behind the gate (C3).
  - `retry_after_source: "local_gate"` — this 429 is a gateway-gate fact, not
    an upstream signal; do not tune the upstream's rate limits.
  - Mitigation: check `in_flight` for leaks, raise per-upstream
    `max_concurrency` only if the upstream truly allows more, or use the
    Admin `POST /api/admin/upstreams/{id}/concurrency/reset` endpoint to
    clear held leases immediately.
- Downstream sees `503 upstream_routes_exhausted` (or `429` for a pure
  rate-limit family): read the error `details` to tell *why* the gateway gave
  up and how to tune:
  - `give_up_reason`: `round_cap` — the routing-round cap bit before the wait
    budget (alignment off / no alignable recovery; raise
    `upstream_route_exhaustion_retry_max_rounds` or check the alignment
    switch); `wait_budget` — the next recovery exceeded
    `upstream_route_exhaustion_retry_max_wait_ms` (raise the budget or lower
    `upstream_transient_route_cooldown_base_seconds`); `no_recovery` — no
    route reported a live recovery to wait for; `alignment_exhausted` — the
    one budget-aligned wait per request was already consumed.
  - `live_recovery_seconds`: the healthy registry's earliest route recovery
    (half-open remaining preferred); the client-side `Retry-After` matches it.
  - `give_up_reason: half_open_busy_cap` + `half_open_busy_count > 0`:
    every candidate was merely being re-probed by another request, not really
    failing — the pool is healthy but the probe (first semantic output) is
    slow. Check the upstream's first-packet latency and the
    `upstream_route_half_open_exclusive_window_ms` /
    `upstream_route_half_open_busy_max_rounds` settings.
  - `physical_attempt_count = 0` should become rare: it now means the pool was
    exclusively half-open-busy (or fully cooling), not that routing silently
    skipped attempts. A persistent large `half_open_busy_count` means probes
    are slow, not that the pool is failing.
  - Reported `cooldown_seconds` (logs / admin snapshots) must never exceed
    `upstream_retry_after_cap_seconds` for upstream-provided values; if a big
    number appears, an upstream 429 carried a huge `Retry-After` that bypassed
    the cap (report it).
  - `last_resort_probe_attempted`: whether the current request itself was
    sent as an early half-open probe (true means the pool was fully cooling
    and the probe reached the upstream).
- Downstream sees `upstream_request_shape_rejected`: routes rejected the same
  request shape (`RequestRejected`). Inspect the request payload and the
  upstream error in `message`; the gateway deliberately stopped replaying to
  protect the pool.
- A single intra-request replay round already happened for transient trips;
  the `Retry-After` header on `upstream_transient_pool_failure` tells the
  client when to retry.
- Downstream sees the **same session** return `503 upstream_routes_exhausted`
  on every "continue" (same error over and over, idle or not), while a brand
  new session works immediately: that is continuation-pin (session-level)
  pinning, not load. Diagnosis:
  - The stuck request carries `previous_response_id`; a new session has none.
  - `details.route_count` / `details.cooled_candidate_count` are **1** (or
    tiny) although the pool has N routes — the continuation contract filtered
    the pool down to the previously-successful key.
  - `details.class_counts` has one class and is byte-identical across
    "continue" attempts — the request keeps hitting the same pinned route.
  - `continuation_pinned = true`, `continuation_candidate_count = 1` and
    (after an escape round ran) `continuation_pin_escaped = true` are now
    surfaced in the terminal error details.
  - Admin → 模型探测 → **能力档案** tab: keys without a Verified profile are
    marked **无法承接续写**; a pool whose other keys were never probed cannot
    fail over (capability-based failover needs a Verified profile per key).
  - Admin upstream runtime snapshot: an `in_flight` that does **not** return
    to 0 while idle means a local concurrency lease leaked (it now self-heals
    after `upstream_local_lease_ttl_seconds`, but a leaked slot lasting
    longer than that is a bug — report it).
  Immediate mitigation without code changes:
  - Run capability probing on **every key** of every upstream so each gets a
    Verified profile; the contract-based failover then works across keys.
  - Raise per-upstream `max_concurrency` from 4 to **16–64** (a pin parks the
    whole session on one key's slots; parallel Codex sub-tasks exhaust 4
    easily).
  - Lower `upstream_transient_route_cooldown_max_seconds` to ~20 so a pinned
    route recovers faster (shortens backoff against genuinely failing
    upstreams as a side effect).
  - Emergency: exit Codex and start a new session (drops
    `previous_response_id`). With the escape round (default on) this should
    no longer be necessary; `upstream_continuation_pin_escape_enabled = false`
    restores the old behavior.
  - If a pool looks dead *only* for existing sessions and `in_flight` never
    returns to 0, restart the gateway to reclaim leaked slots immediately.

## 门户 OIDC 登录 (Portal OIDC)

用 Keycloak / Authentik（或任何 OpenID Connect 提供方，issuer discovery 或
手工端点皆可）让门户用户免密钥登录。**身份一律来自 userinfo，不来自
id_token**；开启 `portal_oidc_verify_id_token` 只是额外验签，不改身份来源。

**前置条件**：必须使用 PostgreSQL 后端。文件模式（无 Postgres）下所有 OIDC
端点返回 503 `oidc_requires_durable_store`，工号+key 登录不受影响。

### 环境变量（端口接线，启动时读取）

| 环境变量 | 默认 | 说明 |
| --- | --- | --- |
| `PORTAL_OIDC_CLIENT_ID` | 空 | OIDC client id（IdP 上注册）。为空时 OIDC 端点 503 |
| `PORTAL_OIDC_CLIENT_SECRET` | 空 | OIDC client secret |
| `PORTAL_OIDC_REDIRECT_URL` | 空 | 回调地址，必须是公开可达的 `https://…/api/portal/oidc/callback` |
| `PORTAL_OIDC_ISSUER_URL` | 空 | issuer 地址；设置后自动拉取 `/.well-known/openid-configuration` 补端点 |
| `PORTAL_OIDC_AUTHORIZATION_ENDPOINT` | 空 | 手工指定授权端点（显式优先于 discovery） |
| `PORTAL_OIDC_TOKEN_ENDPOINT` | 空 | 手工指定令牌端点 |
| `PORTAL_OIDC_USERINFO_ENDPOINT` | 空 | 手工指定 userinfo 端点 |
| `PORTAL_OIDC_SCOPES` | `openid profile email` | 授权 scope |
| `PORTAL_OIDC_AUTH_STYLE` | `auto` | `auto` / `params` / `basic`，控制 client 凭据放 body 还是 Basic header |
| `PORTAL_OIDC_USER_ID_FIELD` | `sub` | userinfo 里的用户标识字段，支持 `a.b.c` 点路径 |
| `PORTAL_OIDC_EMAIL_FIELD` | `email` | userinfo 里的邮箱字段（登录必需） |
| `PORTAL_OIDC_USERNAME_FIELD` | `preferred_username` | 用户名映射（可选） |
| `PORTAL_OIDC_DISPLAY_NAME_FIELD` | `name` | 显示名映射（可选） |

编排（docker compose / k8s）示例：

```yaml
environment:
  - PORTAL_OIDC_CLIENT_ID=chat2responses
  - PORTAL_OIDC_CLIENT_SECRET=please-rotate
  - PORTAL_OIDC_REDIRECT_URL=https://gateway.example.com/api/portal/oidc/callback
  - PORTAL_OIDC_ISSUER_URL=https://idp.example.com/realms/main
```

### 运行时设置（Admin > Settings，签名「门户 / OIDC 登录」，全部立即生效）

| 键 | 默认 | 说明 |
| --- | --- | --- |
| `portal_oidc_enabled` | false | 总开关；关闭时 `/api/portal/oidc/*` 404，不泄露 IdP 地址 |
| `portal_oidc_registration_enabled` | false | 新身份自动注册；关闭时新身份 403 且无记录 |
| `portal_oidc_allowed_email_domains` | 空 | 逗号分隔的允许域名，子域自动放行；空 = 不限制 |
| `portal_session_ttl_seconds` | 86400 | 会话时长（≥60） |
| `portal_oidc_pkce_enabled` | true | 授权请求携带 S256 code_challenge |
| `portal_oidc_verify_id_token` | false | 额外验签 id_token（身份仍来自 userinfo） |

**PostgreSQL 必需**：会话与会话哈希只存在 Postgres；文件模式 503 fail closed。

### Keycloak 样例

1. Realm → Clients → Create client：
   - Client ID: `chat2responses`，Client authentication: ON
   - Valid redirect URIs: `https://gateway.example.com/api/portal/oidc/callback`
   - 保存后复制 Client secret
2. 写环境变量（见上表），重启网关
3. `Admin > Settings` 开启 `portal_oidc_enabled`；需要自动开户时再开
   `portal_oidc_registration_enabled`；有邮箱域名限制就填
   `portal_oidc_allowed_email_domains`

### Authentik 样例

1. Applications → Create：name `chat2responses`；Launch URL 随意
   （OIDC 回调由网关发起）
2. Providers → OAuth2/OpenID Provider：
   - Client type: Confidential；Redirect URIs: 同上
   - Signing Key 随意；签名算法选 RS256 即可
3. 返回 Application → 绑上该 Provider，复制 Client ID / Secret
4. 同 Keycloak 的第 2、3 步

### 行为与安全

- 登录状态由 HttpOnly `portal_session` Cookie 承载，库中只存 SHA-256 哈希
- state 一次性消费（10 分钟 TTL），重放 400
- 禁用用户：同事务清空其全部会话，下一次请求立即失效
- 无绑定 key 的用户登录 403（绝不自动发 key）；绑定发生在
  「密钥管理」页的「绑定企业账号」，或由管理员在「门户用户」页指派
- 身份已绑定他人时 bind 409

## Build The Image

Build the container image directly. The Dockerfile compiles both the frontend
and the backend during the image build.

```bash
docker build -t chat-responses-codex:latest .
```

## Run With Docker

```bash
docker run -d \
  --name chat-responses-codex \
  --restart unless-stopped \
  -p 3001:3001 \
  -e BIND_ADDR=0.0.0.0:3001 \
  -e STATE_PATH=/data/state.json \
  -e LOG_PATH=/logs/chat-responses-codex.log \
  -e RUST_LOG=info \
  -e TZ=Asia/Shanghai \
  -e CAPABILITY_POLICY_BOOTSTRAP_ON_ZERO=true \
  -e ADMIN_USERNAME=admin \
  -e ADMIN_PASSWORD='<admin_password>' \
  -e JWT_SECRET='<jwt_secret_at_least_32_characters>' \
  -v ./data:/data \
  -v ./logs:/logs \
  chat-responses-codex:latest
```

This single-container form is only for file-backed compatibility mode.
For PostgreSQL-backed deployments, use Compose or another orchestrator and provide `DATABASE_URL`.

## Docker Compose

Use this if you want a repeatable local or VM deployment. The checked-in
`docker-compose.yml` is the source of truth for bootstrap wiring and credentials
only; application behavior is maintained in `Admin > Settings`.

The default, single-instance mode does not start Redis:

```bash
docker compose up -d
```

For coordinated replicas, set `REDIS_ENABLED=true` and a unique
`REDIS_KEY_PREFIX` in `.env`, then start the optional Redis profile:

```bash
docker compose --profile redis up -d
```

This command starts Redis and one gateway. Provision additional gateway
replicas through a Compose override or another orchestrator, with unique
container names and non-conflicting host ports. Every replica must use the same
Redis URL and deployment prefix.

The Redis service has no host port. The gateway has no unconditional Redis
dependency because Redis is optional; enabled gateways instead fail fast during
startup until Redis is reachable. Redis does not replace PostgreSQL.

```yaml
services:
  postgres:
    image: postgres:15
    container_name: chat-responses-codex-postgres
    restart: unless-stopped
    environment:
      POSTGRES_DB: chat_responses_codex
      POSTGRES_USER: chat_responses_codex
      POSTGRES_PASSWORD: ${POSTGRES_PASSWORD:?set POSTGRES_PASSWORD in your shell or .env file}
    expose:
      - "5432"
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U chat_responses_codex -d chat_responses_codex"]
      interval: 5s
      timeout: 3s
      retries: 10
      start_period: 5s
    volumes:
      - postgres-data:/var/lib/postgresql/data

  redis:
    image: redis:7-alpine
    profiles: ["redis"]
    restart: unless-stopped
    expose:
      - "6379"
    healthcheck:
      test: ["CMD", "redis-cli", "ping"]
    volumes:
      - redis-data:/data

  gateway:
    image: chat-responses-codex:latest
    build:
      context: .
    container_name: chat-responses-codex
    restart: unless-stopped
    depends_on:
      postgres:
        condition: service_healthy
    ports:
      - "3000:3001"
    environment:
      BIND_ADDR: 0.0.0.0:3001
      DATABASE_URL: postgres://chat_responses_codex@postgres/chat_responses_codex
      PGPASSWORD: ${POSTGRES_PASSWORD:?set POSTGRES_PASSWORD in your shell or .env file}
      LOG_PATH: /logs/chat-responses-codex.log
      RUST_LOG: info
      ADMIN_USERNAME: admin
      ADMIN_PASSWORD: ${ADMIN_PASSWORD:?set ADMIN_PASSWORD in your shell or .env file}
      JWT_SECRET: ${JWT_SECRET:?set JWT_SECRET in your shell or .env file}
      REDIS_ENABLED: ${REDIS_ENABLED:-false}
      REDIS_URL: ${REDIS_URL:-redis://redis:6379}
      REDIS_KEY_PREFIX: ${REDIS_KEY_PREFIX:-chat2responses}
      POSTGRES_POOL_MAX_SIZE: "16"
    volumes:
      - ./logs:/logs

volumes:
  postgres-data:
  redis-data:
```

If you use a `.env` file, copy [`.env.example`](.env.example) to `.env`, review
the bootstrap values, and rotate the secrets before first launch. Configure
behavior under `Admin > Settings` after login.

For Codex client setup, copy [templates/codex/config.toml.example](templates/codex/config.toml.example) and [templates/codex/model-catalog.json](templates/codex/model-catalog.json) into `~/.codex/`, then create `~/.codex/agents/default.toml` from [templates/codex/agents/default.toml.example](templates/codex/agents/default.toml.example). The config template targets Codex CLI `0.146.0`, uses `model_catalog_json = "model-catalog.json"`, and includes `[agents].max_threads` plus `[agents].max_depth`; the catalog and config files must live side by side. The default agent profile must use exactly the same `model` and `model_reasoning_effort` from the same live catalog entry as the main config. Do not leave an older model or reasoning level in this file: Codex loads it independently when starting a subagent and can reject delegation before the gateway receives a request. Because `requires_openai_auth = true`, also run `codex login --with-api-key` and enter the downstream key interactively; the key must not be written into `config.toml`. Run `codex --strict-config doctor --summary` after copying them to validate the loaded configuration.

Generate a secure JWT_SECRET with: `openssl rand -base64 32`

## Reverse Proxy Notes

If you terminate TLS upstream of the gateway:

- Forward `X-Forwarded-For` so downstream IP allowlists work.
- Preserve the `Authorization` header.
- Proxy `/healthz` through unchanged so the Docker health check still works.
- Keep the admin UI off the public internet unless you really need it.

Example Nginx forwarding headers:

```nginx
proxy_set_header Host $host;
proxy_set_header X-Real-IP $remote_addr;
proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
proxy_set_header X-Forwarded-Proto $scheme;
```

## Bootstrap Checklist

1. Start the container.
2. Open `<gateway_origin>/admin`.
3. Log in with the admin credentials.
4. Add upstream keys and model support.
5. Generate a downstream key.
6. Test `GET /v1/models` with `Authorization: Bearer <downstream_key>`.
7. Send one chat request and confirm the upstream receives it.

## Smoke Test

```bash
curl -i <gateway_origin>/healthz
```

```bash
curl -u admin:<admin_password> \
  <gateway_origin>/admin
```

After you create a downstream key:

```bash
curl -s \
  -H "Authorization: Bearer <downstream_key>" \
  <gateway_origin>/v1/models
```

```bash
curl -s \
  -H "Authorization: Bearer <downstream_key>" \
  -H "Content-Type: application/json" \
  -d '{"model":"<model_slug>","messages":[{"role":"user","content":"hello"}]}' \
  <gateway_origin>/v1/chat/completions
```

## Capability Policy And Acceptance

Capability configuration is deployment data, not compiled model classification. Start from `templates/capabilities/current-deployment.example.json`; it contains no credentials or upstream URLs. To add the selected Qwen multimodal route and its semantic image fixture:

```bash
QWEN_VLM_SLUG='<exposed_qwen_slug>' \
IMAGE_FIXTURE_URL='https://example.invalid/fixture.png' \
IMAGE_FIXTURE_EXPECTED_LABEL='<expected_label>' \
scripts/render_live_capabilities.sh --output /tmp/live-capabilities.json
```

Import through the authenticated admin API or use `--import` with `BASE_URL` and `ADMIN_TOKEN`. Import compiles and validates the whole document before persistence and atomic snapshot replacement. An invalid import keeps the last valid revision. Export, exact-route profiles, resolved sources, and manual probes are available under `/api/admin/capabilities/*` and in the admin troubleshooting page.

Policy semantics do not prove relay syntax. Exact `(upstream_id, runtime_model_slug, protocol)` probe evidence controls wire capability, and probes never run on the normal request path. A normal request makes one healthy dispatch attempt, except for the single bounded pre-stream dialect correction defined by a verified profile.

Treat every configured context window as a qualified deployment limit, not a
vendor-advertised maximum. Qualify each exact route and protocol serially at
32k, 64k, 128k, and the configured maximum. At every tier, the text, reasoning,
and read-only tool flows must each pass three consecutive times. Stop at the
first failed tier. A failed 32k tier blocks model qualification. Only the
largest passing tier may be imported as a new explicit capability revision.
Normal traffic never auto-learns a higher context limit, and an ordinary
successful request must not promote deployment data.

After importing the deployment data, run the semantic matrix and installed clients:

```bash
BASE_URL='<gateway_origin>' DOWNSTREAM_ID='<downstream_id>' \
scripts/compatibility_matrix.sh

BASE_URL='<gateway_origin>' DOWNSTREAM_KEY='<downstream_key>' \
MODEL_SLUG='<exposed_model_slug>' scripts/installed_client_smoke.sh
```

The matrix fails on semantic check failures and unpermitted downgrades. The installed-client smoke pins verified CLI versions, executes text and read-only tool tasks in a temporary directory, and never prints the downstream key. Preserve the existing image and data volumes before an upgrade; do not prune images or volumes during rollback preparation.

## Reading A Gateway Error

Every client-visible gateway error carries the same anatomy, so a support
ticket or server log line can be parsed in one pass:

```
[upstream_routes_exhausted] all eligible upstream routes are temporarily
unavailable: transient upstream server errors (1 route, upstream HTTP 503,
upstream=k-api, code=channel_not_found, body="…"); please try again in 12s;
gateway already retried for 0.3s across 1 routing rounds; request_id=…
```

| Piece | Meaning |
|-------|---------|
| `[…]` | Stable machine-readable gateway code (`upstream_routes_exhausted`, `upstream_temporary_unavailable`, `upstream_auth_error`, …). |
| `(…, upstream HTTP 503, upstream=k-api, code=channel_not_found, …)` | Terminal class summary: how many routes failed, each class's most common upstream HTTP status, the upstream display name, the sanitized upstream error-code token, and (only when `upstream_error_body_excerpt_enabled` is on for intranet upstreams) a redacted body excerpt. |
| `please try again in Ns` | How long the gateway itself estimates before an eligible route recovers; honor it before retrying. |
| `request_id=…` | The gateway-generated request id for this request. |

When investigating, take `request_id=…` from the client message and grep the
server runtime log (`LOG_PATH`) for that exact id: the gateway logs every
routing decision, upstream attempt, retry wait and classification under that
request id (`request_id=…` in each structured log line), including the
`error_excerpt` field with the full classification and upstream code.

`error_message` in the admin usage log (Admin > Logs) stores the same
client-facing message, so the code and upstream name are searchable there
without touching the runtime log.

### SSE 解码失败怎么排查 (G5)

流式响应在网关内有两层解码，失败来源不同、错误码不同、日志不同。看到一个
`stream_*` 错误时，先用 `error.code` 区分来源，再按下面的表定位：

| error.code | 来源 | 日志 | 含义 |
|-----------|------|------|------|
| `stream_upstream_sse_parse_error` | SSE 帧解析层 | `upstream SSE decode failed`（`stream_stage=sse_parse`） | 上游发来的帧不是合法 UTF-8（`decode_reason=invalid_utf8`）或 `data:` 载荷不是合法 JSON（`invalid_json`）。典型成因：上游在流里插入错误文本、聚合网关截断 JSON、非标准哨兵值、或中间代理对 body 做了压缩/截断（压缩字节在网关里就是非法 UTF-8）。 |
| `stream_upstream_transport_decode_error` | 传输层 | `upstream stream body read failed`（`stream_stage=proxied/translated/prefetch`） | reqwest 报告 body 解码失败（`error decoding response body`）。 |
| `stream_upstream_read_error` | 传输层 | 同上 | body 读取失败但不是解码失败（连接中断、EOF 等）。 |

排查步骤：

1. 从客户端错误里取 `request_id`，在运行时日志（`LOG_PATH`）里 grep 它。
2. SSE 解析失败看 `decode_reason`、`frame_seq`、`stream_offset`、`payload_len`
   —— 它们指出是第几帧、从流的哪个字节偏移开始坏掉；`payload_excerpt`
   只有在 `upstream_error_body_excerpt_enabled` 打开时才进日志与客户端
   `details`。
3. 传输层失败看 `usable_output_exposed` / `semantic_terminal_observed`：
   这两个字段告诉你坏帧发生时客户端已经拿到了多少内容。
4. 按条数收敛后看管理端上游页的 G4 计数：`sse_bad_frame_skipped_total`、
   `sse_parse_error_total`、`transport_decode_error_total`。

`upstream_error_body_excerpt_enabled` 的适用边界：

- **只在两端都归你运维的内网自有上游上打开**；它会把上游错误 body 的
  有界摘录（默认 200 字符，`upstream_error_body_excerpt_max_chars`）放进
  日志与客户端错误 `details`。摘录经过脱敏（密钥、`Bearer` token、JSON
  密钥对），但上游 body 可能携带用户数据——**公网 / 多租户部署保持关闭**。
- 开关关闭时摘录不进日志、不进客户端响应；错误码、`decode_reason` 等
  结构字段不受影响。

> 注：本项目构建 reqwest 未启用 gzip 特性，网关客户端从不协商也不会
> 解压上游 body；因此压缩流截断在本网关表现为 `sse_parse_error`
> （gzip 字节即非法 UTF-8），而不是 `transport_decode_error`。
> `stream_decode_error_code_split_enabled` 关掉后两个新码都回落为
> `stream_upstream_body_decode_error`（回滚开关，默认开）。

## Operational Notes

- In file-backed compatibility mode, usage logs rotate into archive files next to `STATE_PATH`; archive count and retention are managed under `Admin > Settings`.
- Logs older than the saved retention period are automatically pruned by a background task; setting retention to 0 disables pruning.
- In PostgreSQL mode, usage logs stay in the database and do not rotate into local archive files.
- Runtime logs are appended to `LOG_PATH` and can be mounted to the host with `./logs:/logs`.
- The Docker image exposes a `HEALTHCHECK` that runs the binary's built-in healthcheck mode.
- Per-minute request limiting is enforced at the gateway entry point.
- `daily_token_limit` and `monthly_token_limit` are persisted and shown in the admin UI, but they are not yet enforced by the request path.
- Back up the PostgreSQL data volume or managed database regularly. In PostgreSQL mode, the normalized tables are the source of truth for keys and upstream configuration.
- Keep `REDIS_ENABLED=false` for a single authoritative gateway. Enable Redis before running replicas that must share rate limiting and route health.
