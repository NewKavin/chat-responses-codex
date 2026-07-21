# 多 Key 模型路由韧性设计

## 状态

本设计已经过逐段确认，覆盖多 Key 模型映射、Key 级能力证据、运行时路由健康、错误分类、安全后台刷新和验收测试。

本设计取代 `2026-06-24-auto-sync-model-key-mapping-design.md` 中的后台同步写回语义。旧设计的共享发现逻辑和“成功替换、失败保留”原则继续保留，但旧实现允许已删除 Key 的历史映射重新参与合并，因此不能直接恢复。

本设计扩展但不取代以下既有契约：

- `2026-07-02-gateway-error-visibility-design.md` 的安全错误摘要和结构化错误信封；
- `2026-07-18-codex-reasoning-catalog-design.md` 的证据驱动 capability witness 和保守目录元数据。

对于经过本设计分类为可重试的临时 upstream 失败，本设计取代 `DEPLOYMENT.md` 中“正常请求只做一次健康 dispatch”的旧描述；capability probe 仍不得进入普通请求路径。

## 问题

一个上游账号可以配置多个 API Key，但不同 Key 可能支持不同模型、协议或 reasoning effort。当前实现虽然已经有 `api_key_models`，路由和健康体系仍有多处停留在 upstream 级：

- 管理端“获取模型”只把模型并集写回 `supported_models`，没有重建精确 Key 映射；
- 并集变化时，更新逻辑可能清空 `api_key_models`，使路由退化为盲试 Key；
- `DialectProfileKey` 只有 upstream、模型和协议，没有 Key 身份，能力探测默认取第一个匹配 Key；
- `failure_count`、429 cooldown 和成功恢复都作用于整个 upstream；
- 所有 5xx 都可以立即触发换 Key，最后一个 Key 失败又会给整个 upstream 记持久失败；
- `model is not supported`、协议不支持和 feature 不支持被混在同一错误类别中。

现网请求 `ca62169f-40a6-4573-bfd9-36e59cef4478` 展示了完整故障链：同一 upstream 的 Key A 返回 `400 level "xhigh" not supported`，Key B 返回 `400 model is not supported`，另一个 upstream 随后返回 `503 no available channel for model glm-5.2 under group free`，候选耗尽后下游收到 503。

历史数据还显示 `glm-5.2` 在出现 162 次临时 503 后继续产生 1139 次成功请求。这证明 `no available channel` 是临时容量信号，不能删除模型目录项。

## 目标

- 多 Key upstream 只把请求发给明确支持目标模型和请求能力的 Key。
- 单个 Key、模型或协议路由失败不影响同 upstream 的其他健康路由。
- 普通 5xx、容量不足、限流、凭证错误、模型不支持和请求错误采用不同动作。
- 候选集中仍有未尝试且当前健康的合格路由时，不得提前返回 503；路由在本次真实尝试中首次失败时，仍可在候选耗尽后形成 503。
- 所有候选临时失败时返回稳定、可诊断的 503，同时 `/v1/models` 保持不变。
- 自动发现失败时保留最后成功映射；删除的 Key 永远不会被后台任务恢复。
- capability evidence 细化到 Key 路由，避免一个 Key 的 `xhigh`、stream 或 tools 结论污染其他 Key。
- 失败热路径不再为健康、映射或 capability 结论写持久配置/数据库；既有 usage log 持久化不受影响。
- 文件模式和 PostgreSQL 模式保持等价。

## 非目标

- 不在本阶段把每个 Key 拆成独立 upstream 记录。
- 不把 Key 规范化为新的独立数据库实体。
- 不支持多活 gateway 副本共享 route health。当前部署文档仍要求单活实例。
- 不改变 downstream 白名单、配额计费或 upstream admission 的既有语义。
- 不保证屏蔽所有上游故障；所有合格路由都失败时仍应明确返回错误。
- 不依据一次自由文本错误永久删除模型或能力。

## 方案选择

### 未选：只修复映射写回

只让管理端保存 `api_key_models` 可以减少错误 Key 请求，但无法处理运行一段时间后出现的 503、429、超时和 Key 局部退化，也无法修复 Key 无关的 capability profile。

### 采用：外部 upstream 分组，内部虚拟 deployment

管理和持久化继续以 upstream 为账号边界，内部为每个 `(upstream, key, runtime model, protocol)` 生成虚拟 deployment。映射、能力证据和运行时健康都以该路由为基本单元；账号级配额和并发仍保留在 upstream 级。

该方案获得了 LiteLLM deployment 路由和 Bifrost Key 模型白名单的隔离能力，又避免把现有管理 UI、文件格式、PostgreSQL upstream 表和配额配置整体迁移。

### 未选：一个 Key 一条真实 upstream

该方案的数据模型最纯粹，但会复制 base URL、优先级、配额、模型上下文和管理操作，并改变账号级容量语义。对于当前单活、有限 Key 池部署，迁移成本显著高于收益。

## 核心不变量

1. `supported_models` 是持久能力目录，不是实时健康目录。
2. runtime cooldown 永远不能直接修改 `supported_models` 或 `api_key_models`。
3. 权威 Key 映射存在时，模型未命中的 Key 不得作为 fallback 被盲试。
4. 后台探测失败不是“模型不存在”的证据。
5. 配置写回只能基于写回时仍然存在的当前 Key 集合。
6. 任一成功路由都应立即清除该路由的失败状态；Key 级 half-open 成功还应清除该 Key 的可恢复失败状态。
7. upstream 级失败只能在至少一个合格路由被真实尝试，且该 upstream 对本请求的所有合格路由都失败后产生；仅因映射、能力或已有 cooldown 被过滤不算新的失败。
8. raw API Key、完整 Key 指纹、请求正文和原始上游错误正文不得进入公共错误、usage log 或普通 tracing 字段。

## 持久化 Key 模型映射

### 映射模式

继续使用现有 `api_key_models: Vec<ApiKeyModelConfig>`，但固定以下语义：

- `api_key_models.is_empty()` 表示 legacy/unknown 模式。为兼容历史配置，路由仍可使用原有 Key fallback，同时后台尝试原子补全映射。
- `api_key_models` 非空表示 authoritative 模式。每个当前 Key 都必须有一条记录；`supported_models` 为空的记录表示该 Key 当前未获准参与任何模型路由。
- authoritative 模式中，`keys_for_model()` 未命中时返回空，不回退到全部 Key。
- `supported_models` 必须由 authoritative 映射求并集，不能独立覆盖。

“当前 Key 集合”固定为 `[api_key] + api_keys` 经现有首尾空白裁剪、去空和按首次出现顺序去重后的结果；`api_key_models` 本身不得向该集合补回 Key。标准化逻辑必须保留 authoritative 模式中的空模型记录，移除空 Key 和不属于当前 Key 集合的映射。重复 Key 记录按首次出现位置合并模型并集，重复模型按首次出现顺序去重；若合并后仍为空也保留该记录。authoritative 输入中缺少记录的当前 Key 按当前 Key 顺序补一条空映射。当前“空模型列表直接丢弃”的行为需要移除。

当上游更新请求同时提交 Key 和映射时，后端只接受属于替换后 Key 集合的映射。并集不一致时以后端重新求并集为准，不再清空整个映射。

### 管理端模型发现

`POST` 模型发现接口继续并发探测 Key，但每个结果增加稳定的请求数组索引：

```json
{
  "key_index": 1,
  "models": 3,
  "model_list": ["glm-5.2", "glm-4.7"]
}
```

`key_index` 是请求 `keys` 数组的零基索引，响应 `results` 与请求顺序一致且每个输入位置恰有一项；重复 Key 可以在内部合并 HTTP 探测，但仍要展开为各自索引的结果。失败结果同样包含 `key_index` 和安全错误摘要。共享发现结果、显式 `discover-models` 以及批量创建/更新响应都使用这一契约。前端使用索引关联自己持有的 Key，并在本地显示 `Key #N`；不使用可能碰撞的 Key 前缀，也不要求后端回显秘密。既有 admin model-probe/runtime 响应中的 `key_prefix` 改为匿名 `route_id`。

HTTP 成功但模型列表为空属于不确定发现结果，按失败处理：不能建立第一版权威映射、不能覆盖最后成功映射，也不能累计自动删除确认。管理员仍可在编辑器中显式把某个当前 Key 保存为空映射。

管理端显式保存时：

- 成功 Key 使用新模型列表；
- 失败且已有映射的当前 Key 保留最后成功映射；
- 新增且发现失败的 Key 保存为空映射，暂不参与路由；
- 已删除 Key 的映射立即移除；
- 保存动作本身视为管理员对当前映射的确认。

### Legacy 迁移

后台不得用部分发现结果把 legacy upstream 自动切换为 authoritative。只有全部当前 Key 都得到有效、非空的模型发现结果时，后台才能原子建立第一版权威映射。若长期有 Key 无法通过 `/v1/models` 发现，管理员可以手工确认映射；在此之前保持兼容模式并依靠运行时隔离降低错误影响。

## Key 身份和虚拟 deployment

### Key 指纹

内部使用稳定的单向散列标识 Key。`normalized_api_key` 就是经过现有存储标准化后的 Key：只裁剪首尾空白，不改变大小写或内部字节。

```text
key_fingerprint = SHA-256("chat2responses:key:v1" || NUL || upstream_id || NUL || normalized_api_key)
```

完整指纹只用于内部 Map、持久 capability profile 主键和配置一致性检查。日志使用从完整虚拟 route identity 重新加域散列后截断得到的短 `route_id`，它不能直接截取 Key 指纹；日志不输出完整指纹，也不再输出原始 Key 前缀。Key 轮换自然产生新身份，旧身份的 runtime state 和 profile 会被清理。

### 路由身份

`RouteIdentity` 和 `DialectProfileKey` 增加 `key_fingerprint`。虚拟 deployment 身份为：

```text
(upstream_id, key_fingerprint, runtime_model_slug, wire_protocol)
```

路由构建顺序为：

1. 按现有优先级、配额和协议规则选择 upstream 候选；
2. 根据 authoritative Key 映射生成目标模型的 Key 路由；
3. 使用 Key 级 capability profile 过滤请求所需的 reasoning、stream、tools 等能力；
4. 过滤 Key 级和 route 级 cooldown；
5. 在剩余路由中应用现有轮转和 affinity 规则；
6. 当前 upstream 路由耗尽后再进入下一个 upstream。

账号级请求配额、配置并发和请求成本仍在 upstream 级预留。结果健康和能力证据不再复用该粒度。

## Capability profile

当前 capability probe 会从 `keys_for_model()` 取第一个 Key，导致其他 Key 复用错误证据。新行为必须为每个 mapped Key 独立生成 probe job，并让 probe job 携带 `key_fingerprint`。执行时重新解析当前 Key，找不到精确身份则丢弃过期任务。

Key 特有错误的处理：

- `model is not supported` 记录该 Key-model route 的 mismatch quarantine 并触发定向模型发现；
- `xhigh`、reasoning field、stream 或 tool 不支持只更新该 Key 路由的 capability evidence；
- 较弱请求仍可继续使用该 Key；
- 请求显式需要不支持的能力时，当前路由被排除并尝试其他 Key。

普通请求观察到的 feature/protocol mismatch 先写入有界、默认 TTL 15 分钟的 Key-route 内存负向提示，并去重提交独立 capability probe；它不能在请求热路径直接 upsert 持久 profile。运行时过滤同时读取当前 profile 和该负向提示，probe 的确认结果才写入持久 evidence 并清除提示。只有相同 feature/protocol 的成功证据或配置指纹变化能提前清除对应提示，较弱请求成功不能清除 `xhigh` 提示。probe 仍失败时提示自然到期，允许重新验证，不能形成永久内存封禁。model mismatch 不写 capability profile，而是进入 route quarantine 并触发模型发现。

Codex catalog witness 继续从持久 capability evidence 选择，不读取 runtime cooldown。因此临时故障不会让 reasoning metadata 抖动。若目录广告了 `xhigh`，运行时必须只选择具有相同已验证能力的 Key；这些 Key 全部临时不可用时返回 503，而不是降级到不支持 `xhigh` 的 Key。

### Codex 门户目录与请求路由解耦

下游 `model_allowlist` 非空时，它是该下游 Codex 目录的权威授权集合，而不是对 active upstream 目录的简单交集。网关保留大小写不敏感匹配到的每个 distinct live slug 及其真实 casing；如果两个 live slug 只有大小写不同但可独立路由，两者都必须进入目录。暂时没有任何 live route 的 allowlist 项仍以白名单第一次出现的原始 slug 进入目录，并使用 `reasoning = none`、无图片、无并行工具等保守元数据；只有这类 allowlist-only 重复项才按大小写不敏感键去重。只有 allowlist 为空时，目录模型全集才回退到所有 active upstream 的持久 `route_models()`。这样新增白名单模型后，门户重新拉取一次完整目录即可覆盖全部授权模型，不需要从其他模型复制条目。

默认 Codex `/v1/models` 和门户生成的 `model-catalog.json` 不得包含 `gateway_catalog_witness`、`upstream_id`、`profile_key`、`configuration_id` 或 configuration fingerprint。witness 仍是服务端选择稳定能力元数据的内部证据，但不是 Codex 用户配置，也不形成客户端可编辑的 route 身份。前端生成器还要防御性删除旧网关响应中的 `gateway_catalog_witness`，避免滚动升级时继续暴露内部诊断。

catalog witness 不得约束无 continuation 的新请求。每个新请求都以实际 `requested_features`、runtime hints、模型映射、协议转换、route health、配额和正常 ranking 在所有合格 `(upstream, Key, model, protocol)` route 中选择；同一模型由多个 upstream 支持时，任何实际满足请求能力的 route 都可参与选择和 fallback。目录 witness 的协议、reasoning carrier 或 advertised effort 集合不能把请求预先收窄到一个 upstream/profile/protocol 子集。

已有会话的 continuation 保持 fail-closed 精确绑定。其 upstream、Key 指纹、runtime model、协议、configuration fingerprint、probe schema、tool registry 和 adapter transition 必须继续匹配，不能借“多 upstream 可用”放宽。目录刷新不会让旧 continuation 跨模型或跨 route 迁移；切换模型时仍需新建 Codex 会话。

### 旧 profile 迁移

序列化的 `DialectProfileKey` 为 `key_fingerprint` 增加向后兼容默认值，空字符串表示 legacy。新的 route configuration fingerprint 必须包含 `key_fingerprint`。加载旧 profile 时保留一条仅用于迁移校验的 legacy fingerprint 算法（不含 Key 身份）：

- upstream 当前只有一个 Key：旧 profile 仍匹配 legacy fingerprint 时，可以重新绑定到该 Key 指纹，并改写为包含 Key 身份的新 fingerprint；
- upstream 当前有多个 Key：旧 profile 身份不明确，不能作为 Key 级证据，必须忽略并排队执行逐 Key 探测；
- Key 或模型删除后，对应 profile 必须被清理。

PostgreSQL `dialect_profiles` 增加 `key_fingerprint` 列，主键从 `(upstream_id, runtime_model_slug, protocol)` 迁移为 `(upstream_id, key_fingerprint, runtime_model_slug, protocol)`。文件模式的 capability document 使用新增字段自然区分 profile。

PostgreSQL DDL 必须在同一事务内按以下顺序执行：新增 `TEXT NOT NULL DEFAULT ''` 列、删除旧主键、建立包含 `key_fingerprint` 的新主键，再让新版查询读取该列。空字符串只表示 legacy profile。路由配置加载后，单 Key upstream 把仍满足配置指纹的 legacy 行以新指纹 upsert 后删除空指纹行；多 Key upstream 删除空指纹行并排队重探测。事务失败必须回滚并阻止启动，不能留下半迁移表，也不能因为多 Key 旧 profile 身份冲突而共用证据。

## 运行时健康模型

### 状态层级

运行时维护三个互补层级：

- `KeyHealthKey(upstream_id, key_fingerprint)`：凭证、计费和 Key 全局权限问题；
- `RouteHealthKey(upstream_id, key_fingerprint, runtime_model_slug, protocol)`：模型容量、限流、5xx、网络和超时；
- `RouteSetAggregateKey(upstream_id, runtime_model_slug, protocol)`：只有该 upstream 对本次请求的所有合格路由都不可用时更新，用于跨 upstream 排序和诊断。

现有 upstream admission state 继续负责 in-flight、分钟窗口和配置配额。健康状态使用独立的有界内存 Map，不写入 `PersistedState`。

Key health 和 route health 复用同一状态骨架，至少包含：

```text
consecutive_failures
last_failure_class
last_failure_at
cooldown_until
half_open_probe_in_flight
```

route 恢复证据是一次结果未被分类为 route health 失败；因此 2xx 成功和普通 request 400 都能清除旧的临时 route cooldown，而 model/feature/protocol mismatch 会转入对应 capability 或 quarantine 状态。Key 恢复证据是一次结果既不是凭证/计费失败，也不是 transport/5xx 等不确定结果；只有持有 Key half-open lease 的请求可以据此清除 Key 状态。若 Key half-open 得到不确定临时结果，释放 lease 并保留原 Key 失败阶梯，同时按 exact route 记录本次临时失败。配置变更后清理不再存在的 Key、模型和协议状态；无故障且长期未访问的条目也要定期裁剪。

route-set aggregate 不是独立熔断器，不能覆盖精确 route health，也不能单独排除后来出现的健康路由。每次选择都先重算本请求的合格路由；只要存在健康 route，就忽略旧 aggregate。这样某模型或 `xhigh` 路由集合耗尽不会抑制同 upstream 的其他模型或较弱请求。仅因已有 cooldown 被过滤不能增加 route 或 aggregate 的失败次数。

每个物理 upstream 尝试，包括 same-route retry、fallback 和 hedge，都必须独立取得 upstream admission reservation，并按既有规则计入该账号的请求/并发成本；内部重试不能绕过账号配额。downstream 最终计费和 usage 仍沿用现有成功/终态规则，不因增加 route health 改变。

### 状态迁移

```text
Healthy -> FailedObservation -> Cooling -> HalfOpen -> Healthy
                                      \-> Cooling (failure, longer delay)
```

route 或 Key cooldown 到期后，各自只允许一个请求原子取得 half-open lease。Key lease 在该 Key 的所有 route 间共享；一个请求需要的 Key lease 和 route lease 都取得后才能探测。其他请求继续走替代路由；没有替代路由时返回带 `Retry-After` 的 503，不提前击穿 cooldown。half-open 成功清除相应状态，失败进入更长 cooldown；lease 持有者取消或异常退出时必须释放 lease，并按是否已形成可归因的 upstream 失败决定是否处罚。

连续失败在恢复时立即归零。若距离上次失败超过 10 分钟，也从首次失败阶梯重新开始。runtime 时间计算必须使用 Tokio/单调时钟，不能受系统时间回拨影响。cooldown 使用由 health identity 和失败阶梯经稳定散列派生的确定性 0.8 至 1.2 抖动，不能使用进程随机哈希，避免同一时刻集中恢复并保持测试可重复。

### 默认重试和 cooldown

| 分类 | 范围 | 请求内动作 | 初始 cooldown | 上限 |
|---|---|---|---:|---:|
| `no available channel` / 明确容量不足 | route | 不原地重试，立即换路由 | 15 秒 | 5 分钟 |
| 普通 500/502/503/504、网络、响应头超时 | route | 原路由重试 1 次，退避 300 至 800ms | 10 秒 | 5 分钟 |
| 429 | 默认 exact route；仅结构化错误明确为 Key 全局配额时作用于 Key | 不在原路由等待，记录 `Retry-After` 后换路由 | header 或 30 秒 | header 优先，否则 5 分钟 |
| 401/402/403 | Key | 不重试该 Key | 15 分钟 | 1 小时 |
| 模型不支持/不存在 | route | 隔离并触发定向发现 | 15 分钟 | 1 小时；成功发现或管理员确认可立即恢复 |
| feature 不支持 | exact route capability | 当前请求排除该路由并探测 | 不做整路由 cooldown | capability evidence 决定 |
| 协议/端点不支持 | exact route capability | 切换兼容协议或路由并探测 | 不做模型目录变更 | capability evidence 决定 |
| 普通 400/422、上下文超限 | request | 不处罚路由 | 无 | 无 |

容量、普通 5xx、凭证错误和模型不支持的后续 cooldown 按各自阶梯增长并在上限截断。`Retry-After` 是上游显式约束，不因通用 5 分钟上限而缩短。模型不支持的 targeted discovery 若仍列出该模型则立即解除隔离；若发现失败，隔离保持到 cooldown 到期后的一次 half-open，而不是永久封禁。

一次请求在同 route 上的“初次尝试 + 普通 5xx 短退避重试”合并为一个健康观察：重试成功直接清零，重试仍失败才把 `consecutive_failures` 增加一次并进入 cooldown。hedge 是独立 route 尝试，按自己的最终结果记一次观察。

这里的 429 指真实 upstream HTTP 响应；gateway 自身在 dispatch 前的 admission/concurrency 等待保持既有语义。真实 upstream 429 不再按 `UPSTREAM_RATE_LIMIT_RETRY_ATTEMPTS` 在原 route 内 sleep/retry，该变量及 `UPSTREAM_RATE_LIMIT_MAX_RETRY_AFTER_SECONDS` 保留解析兼容但不再裁剪 route health 的 `Retry-After`，部署模板和文档要标记废弃。

## 错误分类和请求动作

分类器输入为 HTTP status、headers、结构化 upstream error code 和内部可检查的错误正文。优先级为：

1. 结构化 status/code；
2. 已知、窄范围的供应商消息模式；
3. HTTP status 默认分类。

原始正文只用于进程内分类，公共错误和 usage log 继续使用既有安全摘要。

需要新增或拆分的稳定类别：

- `upstream_model_unsupported`
- `upstream_feature_unsupported`
- `upstream_capacity_unavailable`
- `upstream_routes_exhausted`
- `upstream_credentials_exhausted`

既有 `upstream_protocol_unsupported` 保留给真正的协议/端点问题，不能再承载模型不支持。

关键模式：

- 包含 `no available channel` 且指向目标模型/组的 503 归类为 route 容量不足；
- `openai_error` 且外层 status 为 5xx 归类为普通临时 server error；
- 外层 5xx 即使正文嵌套 4xx 也仍按临时 upstream error 处理；
- `model not supported`、`model not found` 归类为 Key-model mismatch；
- `level "xhigh" not supported` 归类为 feature mismatch，不封禁该 Key 的普通请求；
- 无明确能力信号的 400/422 不轮询全部 Key。

请求尝试顺序：

1. 普通 5xx/transport 在同 route 做一次短退避重试；
2. route 仍失败或错误明确要求换路由时，尝试同 upstream 其他合格 Key；
3. 当前 upstream 路由耗尽后尝试其他 upstream；
4. 只在所有候选都结束后生成终态错误。

自动重试和 fallback 只允许发生在尚未向 downstream 发送语义输出时。所有重放复用同一个请求级幂等标识；上游协议支持幂等键时必须透传或派生稳定键。不支持幂等键的上游仍是 at-least-once 语义，可能产生重复推理或存储记录；本阶段不承诺 exactly-once。

终态优先级：

- 至少一个候选是临时失败或处于临时失败类别的既有 cooldown：`503 upstream_routes_exhausted`，`Retry-After` 为最早可恢复时间；已到期但 half-open lease 被占用的候选按 1 秒计；
- 所有候选都是凭证/计费永久错误：`502 upstream_credentials_exhausted`；
- 所有已尝试候选都明确不支持目标模型：`502 upstream_model_unsupported`，并触发定向发现；
- 所有候选都缺少请求明确要求的能力：`400 capability_not_supported`；
- 所有候选都只有协议/端点不兼容：`502 upstream_protocol_unsupported`；
- 其他不含临时失败的混合型 upstream 耗尽：`502 upstream_routes_exhausted`；
- 模型未配置或不在 downstream 白名单：保持既有模型访问错误。

安全 `details` 可以包含尝试路由数量、分类计数、数字 status 和 retry-after，不包含 Key、完整指纹或原始错误正文。日志分别记录 `upstream_status` 和最终 `downstream_status`，避免把“上游 500，候选耗尽后下游 503”误认为状态丢失。

## Upstream aggregate 和热路径持久化

`UpstreamConfig.failure_count` 不再作为逐尝试的持久健康计数参与路由。现有字段保留用于序列化兼容，但选择逻辑改用 runtime route health 和 route-set aggregate；旧的非零值在迁移后被忽略，并可在下一次正常配置持久化时归零。

以下动作不得发生在普通失败热路径：

- 持久化增加 upstream failure count；
- 修改 `supported_models`；
- 修改 `api_key_models`；
- 写 capability profile，除非是独立 probe 对确认结果的写入。

这样既避免一次 Key 失败污染整个 upstream，也消除故障高峰期间的数据库写放大。

## 安全后台刷新

### 调度

重新启用 `UPSTREAM_MODEL_KEY_SYNC_INTERVAL_SECONDS`：

- 默认 900 秒；
- 配置为 0 时禁用，作为运营 kill switch；现有环境解析的 `.max(1)` 必须移除；
- 启动后经过 30 至 90 秒确定性抖动再执行首轮；
- upstream 之间增加抖动；
- 使用全局 semaphore 限制模型发现 HTTP 并发；
- 定向发现任务按 `(upstream, key_fingerprint)` 去重，不能无限排队。

发现任务不阻塞用户请求，也不占用 gateway 配置的推理并发槽。

### 快照和写回

每个任务快照：

```text
upstream_id
base_url
ordered_current_keys
protocols
api_key_models
supported_models
configuration_fingerprint
```

写回前重新读取当前 upstream。任一快照字段发生变化，本轮结果全部丢弃并等待下一轮，不能把旧结果合并进新配置。

写回算法只从写回时的当前 Key 集合开始：

1. 成功的当前 Key 替换为新模型列表；
2. 失败的当前 Key 保留其最后成功映射；
3. 新增且没有最后成功映射的失败 Key 保持空映射；
4. 不在当前集合中的历史 Key 不进入结果；
5. 从最终映射重新求 `supported_models`；
6. 全部 Key 失败时不修改配置和 `last_synced_at`。

该算法禁止“从完整旧映射开始，再追加剩余项”的实现方式，因为那正是已删除 Key 被复活的历史原因。

### 新增和删除模型

- 新模型在一次成功发现后即可加入该 Key 映射；
- 自动删除模型需要两个相隔至少 60 秒的成功发现周期都确认缺失；
- 失败、超时、5xx、无法解析和空模型列表不算缺失确认；
- 管理员显式编辑并保存，或保存一次非空的成功发现结果，属于显式确认，可以立即应用其中的删除；
- Key 配置删除不需要两次确认，必须立即删除对应映射、profile 和 runtime state。

待确认删除计数只保存在内存。重启后重新累计更保守，不会造成错误删除。管理员显式编辑并保存当前映射可以立即删除；保存一次非空的成功发现结果也可以立即应用其中缺失的模型。空发现结果仍按失败处理，不能借此自动清空映射。

待确认键为 `(upstream_id, key_fingerprint, model)`。一次成功发现重新包含模型时立即清零其缺失计数；只有同一配置快照下相隔至少 60 秒的连续成功缺失观察才能达到两次确认。

模型 mismatch 触发定向发现：若发现仍包含模型，则清除 quarantine；若连续确认缺失，则只删除该 Key 的模型映射。模型仍由其他 Key 支持时，目录并集不变。

## 模型目录稳定性

`/v1/models`、Codex catalog 和门户模型列表只从持久能力映射与 downstream 白名单生成，不读取 route cooldown、half-open 或 runtime aggregate，也不在请求路径实时调用 upstream `/v1/models`。当前 `available_models_for_downstream()` 在持久目录为空时同步探测上游的分支需要移除。

因此：

- `no available channel`、普通 5xx、429 和 timeout 不移除模型；
- runtime success/failure 不改变 catalog witness；
- 只有管理员确认或成功发现的确认策略才能修改持久映射；
- capability metadata 可以在 Key 级 probe 获得新证据后变化，但不因短期健康变化抖动。

当目录仍广告模型而所有路由都因容量、5xx、429 或 transport 等临时类别处于 cooldown 时，请求返回 503 和 Retry-After；若所有路由都是 model mismatch quarantine，则按终态规则返回 502 并继续定向发现。两者都不直接修改目录，这是稳定目录与实时健康分层后的预期行为。

legacy upstream 的请求兼容语义保持不变；但若其持久 `supported_models` 也为空，目录暂不广告模型，直到管理员保存或后台完整发现原子建立第一版映射。升级前依赖实时 `/v1/models` 探测的部署必须先完成一次成功发现。

## Streaming、恢复和 hedging

- `StreamCompletionContext` 必须携带完整虚拟 route identity，使流结束后的成功/失败归因到具体 Key-model-protocol。
- 在第一个可用语义事件前失败时，保留现有 stream-to-JSON 和候选 fallback 能力。
- 已向 downstream 发送语义输出后不得把请求重放到另一 route；只记录当前 route 失败并安全终止流。
- downstream 主动取消和 hedge loser 被取消不能计为 upstream route 失败。
- hedged attempts 必须使用不同 route identity；胜者成功清除自己的健康状态，取消的其他尝试不受处罚。
- request-scoped attempted-route set 防止一次请求在 fallback 中重复击中同一虚拟 route。

## 可观测性和安全

每次尝试记录结构化字段：

- request ID、upstream ID、匿名 route ID；
- exposed/runtime model、protocol；
- upstream status、错误分类；
- same-route retry、next-key、next-upstream、cooldown、half-open 等动作；
- cooldown 秒数和候选剩余数量。

终态 usage log 使用稳定错误 category。原始 upstream body 继续经过既有安全摘要边界，不因新增分类器而写入日志或响应。

管理端 runtime snapshot 可以聚合展示：健康 route 数、cooldown route 数、最早恢复时间和分类计数。初始实现不要求新增完整 route 管理 UI；所有 admin response/export 使用安全 DTO，只能展示匿名 `route_id`，不得序列化完整 Key 指纹或 secret-derived prefix。

## 并发和内存边界

- route health 和 Key health 使用与现有 AppState 一致的异步锁，但网络 I/O、sleep 和配置持久化不能持锁执行。
- half-open lease 的检查与设置必须在同一临界区完成。
- runtime state 的最大自然规模受当前配置中的 Key-model-protocol 组合限制；配置变更后主动裁剪，另加空闲 TTL 兜底。
- legacy/unknown 模式可接受任意模型名，因此 route health 和 route-set aggregate 还必须有硬上限：全局默认最多 16384 项、单 upstream 最多 4096 项。达到上限时先淘汰 cooldown 已过期的最久未访问项，再淘汰最久未访问项并 fail-open；不能淘汰仍持有 half-open lease 的项。Key health 只允许为当前配置 Key 建项。
- capability probe 和 model discovery 使用独立的有界队列/并发控制，不能由错误风暴无限扩张。
- route fingerprint 在 routing snapshot 构建时计算并复用，不在每个重试分支重复散列。

## 兼容和迁移

### 上游配置

- 单 Key、无 `api_key_models` 的历史配置继续工作；
- 多 Key legacy 配置继续工作，后台只有在完整发现后才原子切换；
- 已有非空映射按 authoritative 处理；
- authoritative 空模型项从本版本起保留；
- 文件和 PostgreSQL roundtrip 必须保持相同结果。

### Capability 数据

- 文件 capability document 接受缺失 `key_fingerprint` 的旧 profile；
- PostgreSQL 迁移新增列并扩大主键；
- 单 Key profile 可安全重新绑定；
- 多 Key 旧 profile 不作为 Key 级证据，重新探测；
- 迁移失败必须阻止服务带着半迁移 profile 启动，而不能静默共用错误证据。

### 错误 API

OpenAI 和 Anthropic 错误信封形状保持既有设计。只增加稳定 code/category 和安全 details。HTTP 503 仍表示临时无可用路由；上游原始 500 保留在内部数字诊断字段，不直接改变为对客户端不安全的原文。

### 部署边界

route health 保持进程内状态，重启后 fail-open 并重新学习。当前仓库明确禁止多个活跃 gateway 副本共享同一数据库；未来开放多活时，应把 half-open lease 和 cooldown 放入共享存储，并重新评估一致性。

## 测试设计

### 单元测试

- authoritative/legacy `keys_for_model` 语义；
- 空模型映射保留、缺失当前 Key 补空、重复 Key 合并、旧 Key 剔除和并集派生；
- Key 指纹稳定性、Key 轮换和日志脱敏；
- error classifier 对 500 `openai_error`、503 `no available channel`、429、401/403、model mismatch、`xhigh` mismatch 和普通 400 的分类；
- cooldown 阶梯、单调时钟、确定性 jitter、10 分钟 streak reset；
- route/Key half-open 单 lease、取消释放和分类恢复证据；
- terminal error 优先级和 Retry-After 选择。

### 状态和持久化测试

- 文件、PostgreSQL 对 authoritative 空项和 Key 映射的 roundtrip；
- dialect profile PostgreSQL 主键迁移和旧行加载；
- PostgreSQL DDL 迁移失败整笔回滚并阻止启动；
- 单 Key 旧 profile 重新绑定，多 Key 旧 profile 被忽略并重探测；
- 配置更新过滤不属于当前 Key 集合的映射；
- 热路径 route failure 不触发配置持久化。
- feature/protocol 负向提示只存在内存，去重 probe 确认后才持久化 profile。

### 模型发现测试

- 显式发现和批量管理接口按输入顺序返回 `key_index`，model-probe/runtime 只返回匿名 `route_id`；
- 空模型列表按失败处理，重复 Key 仍返回逐索引结果；
- 成功 Key 替换，失败 Key 保留最后成功值；
- 新失败 Key 保持空映射；
- 全失败配置字节级不变；
- legacy 部分成功不切换，全部成功原子切换；
- 并发删除、替换或重排 Key 时丢弃过期结果；
- 删除 Key 不会在后续同步中复活；
- 新增模型一次确认，删除模型两次确认；
- targeted discovery 去重且有界。
- 配置值 0 禁用后台循环，非零值经过启动和 upstream 抖动执行。

### Gateway 集成测试

- 两个 Key 支持不同模型时只请求正确 Key；
- Key A 返回 `no available channel`、Key B 成功时 downstream 成功且只冷却 A 对应 route；
- 普通 500 首次失败后重试原 Key，第二次失败后才换 route；
- 同 route 两次 500 只增加一次连续失败，重试成功则不进入 cooldown；
- 401/403 只隔离失败 Key，其他 Key 和模型可继续成功；
- 429 默认只冷却 exact route，结构化 Key 全局配额才冷却 Key，均遵循 Retry-After；
- `model not supported` 隔离 Key-model route，不立即修改目录；
- 所有候选都明确 `model not supported` 时返回 502 而不是临时 503；
- `xhigh not supported` 只影响需要 xhigh 的请求，普通请求仍能使用该 Key；
- 当前 upstream 全部 route 失败后才更新 aggregate 并尝试下一个 upstream，旧 aggregate 不能屏蔽后来健康的 route 或其他模型；
- 所有候选临时失败返回 503 和最短 Retry-After；
- 所有凭证失败返回 502；
- 尚有未尝试的健康合格候选时不提前返回 503，其中任一候选成功则 downstream 成功；
- cooldown、half-open 和成功恢复期间 `/v1/models` 内容保持不变。
- 持久目录为空时 `/v1/models` 不同步请求 upstream，后台成功发现后才开始广告模型。

### Streaming 和 hedging 测试

- 首个语义事件前的失败可走既有恢复路径并正确归因；
- 已输出部分内容后不重放请求；
- downstream 取消不处罚 route；
- hedge loser 取消不处罚 route；
- request-scoped attempted set 防止重复 route。

### 长时间回归和安全测试

- 使用 Tokio paused time 跨越多个 15 分钟刷新周期，不真实等待；
- 重放现网错误序列并验证最终 fallback；
- 模拟临时 503 后大量成功，验证模型从未被移除；
- 检查客户端错误、usage logs、tracing 和 admin API 不包含完整 Key、完整指纹、prompt、tool arguments 或原始上游正文；
- 检查 capability/admin export 不会因 `DialectProfileKey` 新字段泄漏完整 Key 指纹；
- 执行 Rust 全量测试、前端类型检查/测试/构建和 mock upstream 压力测试。

## 验收标准

1. authoritative 映射建立后，不再向未映射 Key 发送目标模型请求。
2. 网关不会在尚有未尝试的健康且能力兼容 route 时提前结束；其中任一 route 成功则 downstream 成功，非重试型请求错误按其本身返回。
3. 所有 route 临时失败时只返回 `503 upstream_routes_exhausted`，并提供安全 Retry-After。
4. 临时故障、cooldown 和 half-open 不改变 `/v1/models`。
5. 删除 Key 后，后台同步、capability probe 和旧 profile 都不能恢复它。
6. `xhigh` 等 Key 特有能力只影响对应 route。
7. 失败高峰不会产生逐请求持久配置写入。
8. 两个以上刷新周期和 cooldown 恢复周期的确定性测试通过。
9. 既有 OpenAI/Anthropic 错误安全边界和 Codex reasoning catalog 行为不回归。

## 交付顺序

实施计划应按以下依赖顺序拆分，并使用 TDD：

1. 固定映射不变量、空项语义和管理发现 `key_index`；
2. 引入稳定 Key 指纹和 Key-aware capability profile，包括 PostgreSQL 迁移；
3. 建立 route candidate 与运行时健康状态机；
4. 接入错误分类、重试、terminal aggregation、stream 和 hedge 归因；
5. 恢复安全后台刷新和 targeted discovery；
6. 补齐可观测性、安全检查、全量验证和部署文档。

每一阶段都必须保持现有单 Key 配置可用。后台刷新应最后启用，确保精确映射和 route health 已经能够安全消费结果。

## 参考实现

- LiteLLM：同一模型组包含多个 deployment，health/cooldown 按 deployment ID 过滤，配置模型组不因临时 cooldown 被删除。
- Bifrost：每个 Key 具有模型白名单、黑名单、权重和状态，先按模型过滤 Key，再选择和 failover。
- New API：多 Key channel 能单独禁用 Key，但模型能力仍在 channel 级，说明 Key 异构时仅有 Key 轮询不够。
- Portkey：retry、load balance、fallback 和 circuit breaker 以 target 为路由单元。
- Envoy outlier detection：连续失败、失败率、递增 eject 时间、half-open/active recovery 均与持久服务目录分离。
