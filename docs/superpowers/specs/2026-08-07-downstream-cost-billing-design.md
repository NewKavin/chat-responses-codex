# 下游按费用（钱）计费 — 设计文档

日期：2026-08-07
状态：已实现（2026-08-07 增补输入/输出双单价）

## 1. 背景

上一版实现了 `billing_mode = "token"`：按每日 token 消耗限额（滚动 24h 窗口）准入。
用户进一步要求：**最终以费用（钱）计费**，例如每日限额 30 元。
「100 万 tokens 多少钱」是单价，管理员设置的就是每日费用总额度。

## 2. 设计决策

- 保留 `billing_mode = "request" | "token"`。token 模式升级为「按费用」：
  管理员配置「输入/输出每百万 token 单价（分）」+「每日费用限额（分，如 30 元 = 3000 分）」。
- **输入与输出分开计价**：`费用 = input_tokens × 输入单价 / 1_000_000 + output_tokens × 输出单价 / 1_000_000`
  （u128 中间量防溢出，缺失方向按 0 计；在**写入事件时**换算并固化，单价变更不影响历史窗口）。
- 金额一律用**整数分（cents, u64）**存储与计算，避免浮点累计误差；API/前端展示元（÷100）。
- 准入：token 模式下若配置了 `daily_cost_limit_cents` 且至少一个单价 → 按滚动 24h 费用窗口拒绝；
  否则回退 `daily_token_limit`（token 数限额，兼容存量配置）。
- 不做月费用限额（延续上一版「只做每日」的决策）。

## 3. 数据模型

### DownstreamConfig（src/state/types.rs）
字段（均 `#[serde(default)]`）：
```rust
pub input_token_price_per_million_cents: Option<u64>,  // 每百万输入 token 单价（分），如 1000 = 10 元/百万
pub output_token_price_per_million_cents: Option<u64>, // 每百万输出 token 单价（分）
pub daily_cost_limit_cents: Option<u64>,               // 每日费用限额（分），如 3000 = 30 元
```
- 单位说明：字段值 = 1000 表示「100 万 tokens = 1000 分 = 10 元」。
- `cost_billing_mode()`：token 模式 + 至少一个单价 + `daily_cost_limit_cents` 同时存在才启用按金额计费。
- `cost_for_tokens(input, output)`：按双单价分别折算后相加，缺失方向按 0。
- 兼容：`daily_token_limit`（token 数）保留，作为费用限额未配置时的回退。

### UsageLog（src/state/types.rs）
新增：
```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub total_cost_cents: Option<u64>, // 本次请求费用（分）
```
- 网关记录 UsageLog 时换算写入（需要拿到下游单价配置）。
- Postgres `usage_logs` 加列 `total_cost_cents BIGINT NULL`（ADD COLUMN IF NOT EXISTS）。

### Postgres downstreams 表
新增列（ADD COLUMN IF NOT EXISTS，老库自动补列）：
```sql
input_token_price_per_million_cents BIGINT NULL,
output_token_price_per_million_cents BIGINT NULL,
daily_cost_limit_cents BIGINT NULL
```
迁移同时执行 `ALTER TABLE downstreams DROP COLUMN IF EXISTS token_price_per_million_cents;`
（旧单列字段已被双单价替代；线上若仍有该列数据会丢失，本功能上线前该列无存量业务数据）。

## 4. 执行逻辑（mode 驱动，费用优先）

| 位置 | 改造 |
|---|---|
| 内存准入 `reserve_downstream_request`（state.rs:3128） | token 模式：`cost_billing_mode()` → 累加窗口内 `cost_cents` 超限拒绝；否则回退 `daily_token_limit` |
| `record_downstream_tokens`（redis_runtime.rs:258） | 参数 `tokens` 改为写入 `cost_cents`（token_values HSET 存费用分）；幂等键不变 |
| `record_downstream_usage_event`（state.rs:2850） | 内存窗口事件 `DownstreamTokenEvent.tokens` 存 cost_cents（费用优先）；事件结构加语义注释 |
| `build_downstream_token_windows`（usage.rs:89） | 重建时用 `log.total_cost_cents.unwrap_or(0)` 作为事件值（无费用数据回退 0 → 不触发费用限额） |
| `compute_token_usage`（usage.rs:340） | token 模式且费用限额配置：daily 的 used/limit 改为费用口径（分）；否则维持 token 口径；门户据此展示 |
| `downstream_billing_info`（gateway.rs:1490） | token 模式且 `cost_billing_mode()`：`cost_for_tokens(prompt_tokens, completion_tokens)` 计算本次费用（分）写入 `total_cost_cents` |

### DownstreamTokenEvent 语义
```rust
pub(super) struct DownstreamTokenEvent {
    pub created_at: u64,
    /// 费用优先：值为 cost_cents（分）；无费用配置时回退为 token 数
    pub tokens: u64,
}
```
（字段名保留 `tokens` 减少改动，语义按是否有费用限额解释。）

## 5. 管理 API

- `admin_update_downstream`：支持 `input_token_price_per_million_cents`、`output_token_price_per_million_cents`、
  `daily_cost_limit_cents`（数值→Some，null→None，缺省不动）。
- `admin_batch_set_downstream_mode`：请求体加 `daily_cost_limit_cents: Option<Option<u64>>`、
  `input_token_price_per_million_cents: Option<Option<u64>>`、`output_token_price_per_million_cents: Option<Option<u64>>`。

## 6. 前端

- `Downstreams.vue`（按金额表单，图标 + 人性化说明）：
  - 「输入价格（元/百万 Token）」`inputTokenPricePerMillion`（Coins 图标）：用户发给模型的 Token 单价；
  - 「输出价格（元/百万 Token）」`outputTokenPricePerMillion`（ArrowUpFromLine 图标）：模型回复生成的 Token 单价；
  - 「每日金额上限（元）」`dailyCostLimit`（Wallet 图标）：滚动 24h 窗口费用上限；
  - 校验：至少填写输入或输出价格中的一项；提交元 ×100 转分，编辑回填 ÷100；
  - 列表限额列：金额计费显示「每日 ¥X · 输入 ¥A · 输出 ¥B/百万」；
  - 批量设置 dialog：同样提供输入/输出单价 + 每日金额上限（带图标）。
- `types/index.ts`：`DownstreamConfig` 加 `input_token_price_per_million_cents` / `output_token_price_per_million_cents`；
  `UsageLog` 加 `total_cost_cents`。
- `api/admin.ts`：batch payload 加字段。
- 门户（portal_overview / portal_quota）：token 模式展示费用（元）。

## 7. 测试计划

- `tests/unit/billing.rs`：双单价独立计价、缺失单价按 0、无单价为 0、`cost_billing_mode` 判定。
- `tests/downstream_quota.rs`：费用限额准入（滚动 24h 费用窗口、滑出释放）、
  无费用限额回退 token 限额、单价换算正确性。
- `tests/admin_downstreams.rs`：update/batch 支持双单价字段（数值/null/缺省）。
- `tests/postgres_roundtrip.rs`：downstreams 双单价列 roundtrip + 老库自动补列（DROP 旧列）；usage_logs total_cost_cents 列。
- `tests/redis_runtime.rs`：费用计费下游 token keys 仍按 24h 保留窗口（并行套件）。
- 前端：表单/批量交互（admin-ui.spec.ts 断言双单价字段）。

## 8. 验证部署
同上一版：全量门禁 → build-package-image → 只换 gateway 容器 → 实测（配费用限额下游 → 门户展示 → 请求触发 429）→ push。
