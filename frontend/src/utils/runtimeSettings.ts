import type { RuntimeSettingKey, RuntimeSettings } from '@/types'

export type RuntimeSettingGroupId =
  | 'general'
  | 'discovery'
  | 'routing'
  | 'concurrency'
  | 'http'
  | 'logs'
  | 'observability'
  | 'portal'

export type RuntimeSettingApplyMode = 'immediate' | 'restart'
export type RuntimeSettingControl = 'text' | 'switch' | 'number' | 'number-list'

export interface RuntimeSettingGroup {
  id: RuntimeSettingGroupId
  label: string
}

export interface RuntimeSettingField {
  key: RuntimeSettingKey
  group: RuntimeSettingGroupId
  label: string
  apply: RuntimeSettingApplyMode
  control: RuntimeSettingControl
  unit?: string
  min?: number
  max?: number
  step?: number
  integer?: boolean
  maxLength?: number
  /** Text fields whose empty string is meaningful (empty = unrestricted). */
  allowEmpty?: boolean
  description?: string
}

export type RuntimeSettingsValidationErrors = Partial<Record<RuntimeSettingKey, string>>

const MAX_SAFE_INTEGER = Number.MAX_SAFE_INTEGER
const MAX_U32 = 4_294_967_295
const MAX_PROBE_DELAY_MS = 60_000

export const runtimeSettingGroups: RuntimeSettingGroup[] = [
  { id: 'general', label: '通用' },
  { id: 'discovery', label: '发现与探测' },
  { id: 'routing', label: '路由策略' },
  { id: 'concurrency', label: '并发恢复' },
  { id: 'http', label: 'HTTP 与流式' },
  { id: 'logs', label: '日志' },
  { id: 'observability', label: '可观测性' },
  { id: 'portal', label: '门户 / OIDC 登录' }
]

export const runtimeSettingFields: RuntimeSettingField[] = [
  {
    key: 'app_name',
    group: 'general',
    label: '应用名称',
    apply: 'immediate',
    control: 'text',
    maxLength: 120
  },
  {
    key: 'admin_upstream_timeout_seconds',
    group: 'general',
    label: '管理请求超时',
    apply: 'immediate',
    control: 'number',
    unit: '秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'troubleshooting_check_timeout_seconds',
    group: 'general',
    label: '故障排查单项超时',
    apply: 'immediate',
    control: 'number',
    unit: '秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'model_probe_refresh_interval_seconds',
    group: 'discovery',
    label: '模型探测刷新间隔',
    apply: 'immediate',
    control: 'number',
    unit: '秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'upstream_model_auto_discovery_enabled',
    group: 'discovery',
    label: '自动发现上游模型',
    apply: 'restart',
    control: 'switch'
  },
  {
    key: 'upstream_model_key_sync_interval_seconds',
    group: 'discovery',
    label: '模型 Key 同步间隔',
    apply: 'restart',
    control: 'number',
    unit: '秒',
    min: 0,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'capability_probe_queue_capacity',
    group: 'discovery',
    label: '能力探测队列容量',
    apply: 'restart',
    control: 'number',
    unit: '项',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'capability_probe_request_timeout_seconds',
    group: 'discovery',
    label: '能力探测请求超时',
    apply: 'immediate',
    control: 'number',
    unit: '秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'capability_probe_reasoning_timeout_seconds',
    group: 'discovery',
    label: '思考档位探测超时',
    apply: 'immediate',
    control: 'number',
    unit: '秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'capability_probe_concurrency',
    group: 'discovery',
    label: '能力探测并发数',
    apply: 'immediate',
    control: 'number',
    unit: '路',
    min: 1,
    max: MAX_U32,
    description: '并行执行自动能力探测的最大数量。过高会占用上游额度，过低会拖慢探测覆盖。'
  },
  {
    key: 'automatic_capability_probes_enabled',
    group: 'discovery',
    label: '自动能力探测',
    apply: 'immediate',
    control: 'switch',
    description: '开启后会周期性对所有下游可见模型自动探测（消耗 token）'
  },
  {
    key: 'model_case_insensitive_matching',
    group: 'routing',
    label: '模型名大小写不敏感匹配',
    apply: 'immediate',
    control: 'switch',
    description: '下游请求的模型名与上游支持列表匹配时忽略大小写（如 GPT-4o 与 gpt-4o）。'
  },
  {
    key: 'tool_call_merge_strict',
    group: 'routing',
    label: '工具调用合并严格模式',
    apply: 'immediate',
    control: 'switch',
    description: '严格模式：同一消息内多次工具调用按更严格规则合并与校验，避免重复工具调用。'
  },
  {
    key: 'tool_arguments_strict',
    group: 'routing',
    label: '工具参数严格模式',
    apply: 'immediate',
    control: 'switch',
    description: '严格模式：工具参数缺失或类型不符时拒绝请求；关闭后宽松降级为可用的默认值。'
  },
  {
    key: 'upstream_rate_limit_default_retry_seconds',
    group: 'routing',
    label: '限流默认重试等待',
    apply: 'immediate',
    control: 'number',
    unit: '秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'routing_affinity_enabled',
    group: 'routing',
    label: '路由亲和',
    apply: 'immediate',
    control: 'switch'
  },
  {
    key: 'routing_affinity_ttl_seconds',
    group: 'routing',
    label: '路由亲和有效期',
    apply: 'immediate',
    control: 'number',
    unit: '秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'routing_affinity_escape_pressure_ratio',
    group: 'routing',
    label: '亲和逃逸压力比',
    apply: 'immediate',
    control: 'number',
    unit: '倍',
    min: 1,
    max: 1_000_000,
    step: 0.1,
    integer: false
  },
  {
    key: 'upstream_hedge_enabled',
    group: 'routing',
    label: '上游对冲请求',
    apply: 'immediate',
    control: 'switch'
  },
  {
    key: 'upstream_hedge_delay_ms',
    group: 'routing',
    label: '首次对冲延迟',
    apply: 'immediate',
    control: 'number',
    unit: '毫秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'upstream_hedge_interval_ms',
    group: 'routing',
    label: '后续对冲间隔',
    apply: 'immediate',
    control: 'number',
    unit: '毫秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'upstream_hedge_max_extra_attempts',
    group: 'routing',
    label: '最大额外对冲次数',
    apply: 'immediate',
    control: 'number',
    unit: '次',
    min: 0,
    max: MAX_U32
  },
  {
    key: 'upstream_same_route_retry_enabled',
    group: 'routing',
    label: '同路由重试',
    apply: 'immediate',
    control: 'switch'
  },
  {
    key: 'upstream_transient_same_route_retry_enabled',
    group: 'routing',
    label: '瞬态 5xx 同路由快速重试',
    apply: 'immediate',
    control: 'switch',
    description: 'TransientServer 502/503/504 在进入 failover 前对同一路由快速重试一次（退避 200–500ms，尊重上游 Retry-After，上限 2s）。'
  },
  {
    key: 'upstream_shared_host_failure_domain_enabled',
    group: 'routing',
    label: '同主机故障域',
    apply: 'immediate',
    control: 'switch',
    description: '同一上游 host（多 key）的瞬态失败并入同一故障域，不逐 key 升级冷却；Credentials/key 配额类错误仍按 key 独立冷却。'
  },
  {
    key: 'upstream_common_mode_same_host_transient_enabled',
    group: 'routing',
    label: '同主机瞬态计入共模',
    apply: 'immediate',
    control: 'switch',
    description: '同一 host 的 TransientServer/EdgeProxyError 计入共模 streak；请求形状类 RequestRejected 仍保持跨 host 语义（刻意设计）。'
  },
  {
    key: 'upstream_capacity_failure_cooldown_enabled',
    group: 'routing',
    label: '容量类失败冷却路由',
    apply: 'immediate',
    control: 'switch',
    description: '关闭（默认）：上游 429 / 本地并发闸门拒绝这类容量类失败只记观测、不写路由/Key 冷却、不推进失败计数——「我好着呢，只是现在满了」，冷却会把客户端本就正确的重试循环锁死成 upstream_routes_exhausted。开启则回到旧行为（按 30s 本地曲线冷却）。'
  },
  {
    key: 'upstream_transient_route_cooldown_base_seconds',
    group: 'routing',
    label: '瞬时错误基础冷却',
    apply: 'immediate',
    control: 'number',
    unit: '秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'upstream_transient_route_cooldown_max_seconds',
    group: 'routing',
    label: '瞬时错误最大冷却',
    apply: 'immediate',
    control: 'number',
    unit: '秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'upstream_transient_route_cooldown_max_step',
    group: 'routing',
    label: '瞬时错误冷却升级步数上限',
    apply: 'immediate',
    control: 'number',
    unit: '级',
    min: 1,
    max: 8,
    description: '非半开失败允许的退避升级级数上限（默认 3）。无此上限时 base << (step-1) 会无限增长，最终超过轮间等待预算而必现路由耗尽。'
  },
  {
    key: 'upstream_route_health_half_open_ttl_seconds',
    group: 'routing',
    label: '半开探测有效期',
    apply: 'immediate',
    control: 'number',
    unit: '秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'upstream_route_health_enforcement_enabled',
    group: 'routing',
    label: '路由健康拦截',
    apply: 'immediate',
    control: 'switch',
    description:
      '关闭后路由健康只记录不阻断：冷却与半开状态照常统计，但不再拦截请求，每个请求都真实发往上游，不再因为路由健康冷却而本地拦截成 429/503（upstream_attempted_count 保持大于 0）；上游 502 在多路由/共享网关场景下以 502 直达客户端。用于上游故障时最大化争取上游资源；默认开启。'
  },
  {
    key: 'upstream_route_half_open_exclusive_window_ms',
    group: 'routing',
    label: '半开独占窗口',
    apply: 'immediate',
    control: 'number',
    unit: '毫秒',
    min: 0,
    max: 600_000,
    description: '路由复检期间独占的最长时间，超过后其它请求可并发进入；0 表示不独占。'
  },
  {
    key: 'upstream_route_half_open_busy_max_rounds',
    group: 'routing',
    label: '半开占用最大轮数',
    apply: 'immediate',
    control: 'number',
    unit: '轮',
    min: 1,
    max: 100,
    description: '整池都在复检时，请求最多重试的轮数（不占用普通重试轮数）。'
  },
  {
    key: 'upstream_retry_after_cap_seconds',
    group: 'routing',
    label: '上游 Retry-After 上限',
    apply: 'immediate',
    control: 'number',
    unit: '秒',
    min: 1,
    max: 3_600,
    description: '上游 429/503 携带的 Retry-After 超过该值时按该值封顶。'
  },
  {
    key: 'upstream_retry_after_cooldown_cap_seconds',
    group: 'routing',
    label: '上游 Retry-After 冷却上限',
    apply: 'immediate',
    control: 'number',
    unit: '秒',
    min: 1,
    max: 300,
    description: '上游 Retry-After 只是给客户端的重试建议，不是网关摘除路由的时长；该值限定其参与本地冷却的封顶，避免单个大值吃穿轮间等待预算。'
  },
  {
    key: 'upstream_credentials_first_strike_seconds',
    group: 'routing',
    label: '凭证首次失败冷却',
    apply: 'immediate',
    control: 'number',
    unit: '秒',
    min: 1,
    max: 3_600,
    description: '401/403 第一次只短暂隔离 key，连续失败才升级到 15 分钟以上。'
  },
  {
    key: 'upstream_error_body_excerpt_enabled',
    group: 'observability',
    label: '错误正文摘录',
    apply: 'immediate',
    control: 'switch',
    description: '开启后客户端错误消息尾部会带上脱敏后的上游错误正文（剥 key/token）。仅建议内网自有上游时开启，公网/多租户保持关闭。'
  },
  {
    key: 'upstream_error_body_excerpt_max_chars',
    group: 'observability',
    label: '正文摘录最大字符数',
    apply: 'immediate',
    control: 'number',
    unit: '字符',
    min: 50,
    max: 2_000,
    description: '正文摘录的最大长度，超出部分以省略号截断。'
  },
  {
    key: 'upstream_route_exhaustion_retry_enabled',
    group: 'routing',
    label: '路由耗尽后重试',
    apply: 'immediate',
    control: 'switch'
  },
  {
    key: 'upstream_route_exhaustion_retry_max_wait_ms',
    group: 'routing',
    label: '路由耗尽最大等待',
    apply: 'immediate',
    control: 'number',
    unit: '毫秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'upstream_route_exhaustion_retry_max_rounds',
    group: 'routing',
    label: '路由耗尽最大轮次',
    apply: 'immediate',
    control: 'number',
    unit: '轮',
    min: 1,
    max: MAX_U32
  },
  {
    key: 'upstream_route_exhaustion_budget_alignment_enabled',
    group: 'routing',
    label: '路由耗尽预算对齐等待',
    apply: 'immediate',
    control: 'switch',
    description:
      '轮数上限打满但 live 瞬态恢复仍在剩余时间预算内时，允许多等一次对齐等待后再放弃；429 族耗尽与关闭开关时维持原行为。'
  },
  {
    key: 'upstream_route_exhaustion_alignment_truncated_enabled',
    group: 'routing',
    label: '预算对齐截断重试',
    apply: 'immediate',
    control: 'switch',
    description: '轮数打满但仍在剩余时间预算内时，把最后一次等待截断到剩余预算后作为半开探测再打一次，而不是直接放弃。'
  },
  {
    key: 'upstream_transient_last_resort_probe_enabled',
    group: 'routing',
    label: '全冷却兜底探测',
    apply: 'immediate',
    control: 'switch',
    description:
      '所有候选路由都处于冷却且本请求零物理尝试时，把当前请求作为最后手段探针，提前半开最早恢复的路由并真实发送；便于上游恢复后下一请求立即成功。'
  },
  {
    key: 'upstream_common_mode_breaker_threshold',
    group: 'routing',
    label: '请求拒绝共模熔断阈值',
    apply: 'immediate',
    control: 'number',
    unit: '条',
    min: 0,
    max: 64,
    description: 'RequestRejected（请求形状问题）复读熔断：不同路由连续相同失败达到阈值即停止重放。0 禁用。'
  },
  {
    key: 'upstream_common_mode_transient_threshold',
    group: 'routing',
    label: '瞬态共模熔断阈值',
    apply: 'immediate',
    control: 'number',
    unit: '条',
    min: 0,
    max: 64,
    description: 'TransientServer/EdgeProxyError（5xx/网关错误）跨不同上游 host 连续相同失败达到阈值时，先延迟重放一轮，仍失败才返回 502 upstream_transient_pool_failure。同 host 多 key 不累计。0 禁用。'
  },
  {
    key: 'upstream_continuation_pin_escape_enabled',
    group: 'routing',
    label: '续写路由逃生',
    apply: 'immediate',
    control: 'switch',
    description:
      '当上次成功的那条路由不可用时，允许把会话历史净化后转移到其它可用路由；关闭后该会话只能等原路由恢复。'
  },
  {
    key: 'upstream_local_lease_ttl_seconds',
    group: 'routing',
    label: '本地并发租约上限（秒）',
    apply: 'immediate',
    control: 'number',
    unit: '秒',
    min: 1,
    max: MAX_U32,
    description:
      '兜底回收未正常归还的并发槽；小于单次请求最长时长会误回收，勿低于流最大时长。'
  },
  {
    key: 'upstream_lease_stale_after_ms',
    group: 'concurrency',
    label: '并发租约判停滞超时',
    apply: 'immediate',
    control: 'number',
    unit: '毫秒',
    min: 1_000,
    max: MAX_SAFE_INTEGER,
    description: '并发租约被判为停滞的超时。必须 >= 本地租约心跳间隔（ttl/3）的 2 倍，否则长请求的租约会在心跳前被误回收；后端会拒绝不满足该关系的组合。'
  },
  {
    key: 'default_upstream_max_concurrency',
    group: 'concurrency',
    label: '新建上游每 Key 默认最大并发',
    apply: 'immediate',
    control: 'number',
    unit: '路',
    min: 1,
    max: MAX_U32
  },
  {
    key: 'upstream_account_queue_enabled',
    group: 'concurrency',
    label: '并发饱和排队',
    apply: 'immediate',
    control: 'switch',
    description: '上游账号并发槽位饱和时排队等待而不是立即拒绝；关闭后饱和即返回限流错误。'
  },
  {
    key: 'upstream_account_queue_max_depth',
    group: 'concurrency',
    label: '排队最大深度',
    apply: 'immediate',
    control: 'number',
    unit: '条',
    min: 1,
    max: MAX_U32,
    description: '单账号排队请求的最大深度，超出部分的请求快速失败。'
  },
  {
    key: 'upstream_account_queue_max_wait_ms',
    group: 'concurrency',
    label: '排队最大等待（静态下限）',
    apply: 'immediate',
    control: 'number',
    unit: '毫秒',
    min: 100,
    max: MAX_SAFE_INTEGER,
    description: '排队请求等待槽位的最大时长。开启自适应预算后此项是下限：实际预算 = clamp(观测 p95 持有时长 × 预算系数, 此项, 预算上限)，系数与上限均可单独配置。'
  },
  {
    key: 'upstream_account_queue_poll_interval_ms',
    group: 'concurrency',
    label: '排队轮询间隔',
    apply: 'immediate',
    control: 'number',
    unit: '毫秒',
    min: 10,
    max: MAX_SAFE_INTEGER,
    description:
      '排队请求检查槽位是否释放的间隔。队列查询的是真正执行并发上限的后端：启用 Redis 时每个等待者每次轮询都是一次 Redis 往返，调大可用队列响应速度换取更低的 Redis 压力。不得超过排队最大等待。'
  },
  {
    key: 'upstream_account_queue_adaptive_budget_enabled',
    group: 'concurrency',
    label: '排队预算自适应',
    apply: 'immediate',
    control: 'switch',
    description: '用租约持有时长观测（p95 × 预算系数）动态计算排队预算，替代固定静态等待。关闭则回到固定静态等待。'
  },
  {
    key: 'upstream_account_queue_skip_when_doomed_enabled',
    group: 'concurrency',
    label: '注定失败时跳过排队',
    apply: 'immediate',
    control: 'switch',
    description: '开启时，若中位（p50）持有时长已超过等待下限，则判定「等待注定失败」，直接快速失败而不排队（不会向上游发请求）。慢模型 + 单账号争抢的部署应关闭此项：宁可排队等槽位，也不要被网关本地直接拒绝。'
  },
  {
    key: 'upstream_account_queue_adaptive_budget_factor',
    group: 'concurrency',
    label: '排队预算系数',
    apply: 'immediate',
    control: 'number',
    min: 1,
    max: 100,
    step: 0.1,
    integer: false,
    description: '自适应预算的放大系数：预算 = clamp(观测 p95 持有时长 × 此系数, 等待下限, 预算上限)。默认 1.5。'
  },
  {
    key: 'upstream_account_queue_adaptive_budget_ceiling_ms',
    group: 'concurrency',
    label: '排队预算上限',
    apply: 'immediate',
    control: 'number',
    unit: '毫秒',
    min: 100,
    max: MAX_SAFE_INTEGER,
    description: '自适应排队预算的硬上限，必须不小于「排队最大等待」下限。内网慢模型通常需要显著高于默认 60s。'
  },
  {
    key: 'upstream_local_gate_max_wait_ms',
    group: 'concurrency',
    label: '本地闸门最大等待',
    apply: 'immediate',
    control: 'number',
    unit: '毫秒',
    min: 100,
    max: 60_000,
    description: '本地并发闸门（pre-dispatch 租约）等待槽位的时长上限，超时放弃本轮候选。'
  },
  {
    key: 'upstream_local_gate_fast_fail_enabled',
    group: 'concurrency',
    label: '闸门快速失败',
    apply: 'immediate',
    control: 'switch',
    description: '闸门满且无排队空间时快速失败，不占用轮间重试预算。'
  },
  {
    key: 'stream_decode_error_code_split_enabled',
    group: 'observability',
    label: '解码错误拆分码',
    apply: 'immediate',
    control: 'switch',
    description: '开启后传输层与 SSE 解析的解码失败返回不同错误码（transport / sse_parse），关闭回落为统一旧码。'
  },
  {
    key: 'stream_max_skipped_bad_frames',
    group: 'http',
    label: '坏帧跳过上限',
    apply: 'immediate',
    control: 'number',
    integer: true,
    min: 0,
    max: 1_000,
    description: '已有可用输出后，流最多跳过多少个坏帧再报错（默认 8）。0 表示首个坏帧即失败。'
  },
  {
    key: 'portal_oidc_enabled',
    group: 'portal',
    label: '允许 OIDC 登录',
    apply: 'immediate',
    control: 'switch',
    description:
      '开启后门户登录页显示「使用企业账号登录」。关闭时 OIDC 端点返回 404，工号+key 登录不受影响。'
  },
  {
    key: 'portal_oidc_registration_enabled',
    group: 'portal',
    label: '允许新身份自动注册',
    apply: 'immediate',
    control: 'switch',
    description:
      'OIDC 身份首次出现时自动创建门户用户。关闭时新身份返回 403 且不留下任何用户记录；存量用户通过「绑定企业账号」完成迁移。'
  },
  {
    key: 'portal_oidc_allowed_email_domains',
    group: 'portal',
    label: '邮箱域名白名单',
    apply: 'immediate',
    control: 'text',
    allowEmpty: true,
    description: '逗号分隔的允许域名（如 example.com），空表示不限制。子域自动放行。'
  },
  {
    key: 'portal_session_ttl_seconds',
    group: 'portal',
    label: '门户会话时长',
    apply: 'immediate',
    control: 'number',
    unit: '秒',
    min: 60,
    max: MAX_SAFE_INTEGER,
    description: 'OIDC 门户会话的存活时间，过期后需重新登录。'
  },
  {
    key: 'portal_oidc_pkce_enabled',
    group: 'portal',
    label: '启用 PKCE',
    apply: 'immediate',
    control: 'switch',
    description: '开启后 authorization 请求携带 code_challenge（S256）。关闭以兼容不支持的 IdP，身份仍来自 userinfo。'
  },
  {
    key: 'portal_oidc_verify_id_token',
    group: 'portal',
    label: '验签 id_token',
    apply: 'immediate',
    control: 'switch',
    description:
      '是否校验并解析 id_token（签名、iss、aud、nonce）。默认关闭：身份一律来自 userinfo，与 new-api 保持一致。'
  },
  {
    key: 'upstream_local_gate_distinct_error_code_enabled',
    group: 'concurrency',
    label: '闸门独立错误码',
    apply: 'immediate',
    control: 'switch',
    description: '本地闸门拒绝使用独立错误码（区别于真实上游限流），便于监控区分是本网关的本地排队而非上游限流。'
  },
  {
    key: 'downstream_lease_ttl_seconds',
    group: 'concurrency',
    label: '下游租约有效期',
    apply: 'restart',
    control: 'number',
    unit: '秒',
    min: 60,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'upstream_concurrency_recovery_max_wait_ms',
    group: 'concurrency',
    label: '并发恢复最大等待',
    apply: 'immediate',
    control: 'number',
    unit: '毫秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'upstream_concurrency_recovery_max_rounds',
    group: 'concurrency',
    label: '并发恢复最大轮次',
    apply: 'immediate',
    control: 'number',
    unit: '轮',
    min: 1,
    max: MAX_U32
  },
  {
    key: 'upstream_concurrency_probe_delays_ms',
    group: 'concurrency',
    label: '并发探测延迟序列',
    apply: 'immediate',
    control: 'number-list',
    unit: '毫秒'
  },
  {
    key: 'upstream_http_pool_max_idle_per_host',
    group: 'http',
    label: '每主机最大空闲连接',
    apply: 'restart',
    control: 'number',
    unit: '条',
    min: 8,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'upstream_user_agent',
    group: 'http',
    label: '上游 User-Agent',
    apply: 'restart',
    control: 'text',
    maxLength: 512
  },
  {
    key: 'upstream_connect_timeout_seconds',
    group: 'http',
    label: '上游连接超时',
    apply: 'restart',
    control: 'number',
    unit: '秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'upstream_response_header_timeout_seconds',
    group: 'http',
    label: '响应头超时',
    apply: 'restart',
    control: 'number',
    unit: '秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'upstream_stream_keepalive_interval_seconds',
    group: 'http',
    label: '流式保活间隔',
    apply: 'restart',
    control: 'number',
    unit: '秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'upstream_stream_idle_timeout_seconds',
    group: 'http',
    label: '流式空闲超时',
    apply: 'immediate',
    control: 'number',
    unit: '秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'upstream_stream_max_duration_seconds',
    group: 'http',
    label: '流式最大时长',
    apply: 'restart',
    control: 'number',
    unit: '秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'upstream_first_semantic_output_timeout_seconds',
    group: 'http',
    label: '首个语义输出超时',
    apply: 'immediate',
    control: 'number',
    unit: '秒',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'upstream_first_output_warn_after_seconds',
    group: 'http',
    label: '首字静默告警阈值',
    apply: 'immediate',
    control: 'number',
    unit: '秒',
    min: 1,
    max: MAX_SAFE_INTEGER,
    description: '首字超过该时长仍未输出时打 warn 告警并在在途请求列表高亮（不改 55 分钟超时）'
  },
  {
    key: 'gateway_request_body_limit_mb',
    group: 'http',
    label: '网关请求体上限',
    apply: 'restart',
    control: 'number',
    unit: 'MiB',
    min: 1,
    max: 4_096,
    description: '限制 /v1/chat/completions、/v1/responses、/v1/messages 等入口的请求体大小，超出返回 413。修改后需重启生效。'
  },
  {
    key: 'usage_log_archive_max_files',
    group: 'logs',
    label: '日志归档文件上限',
    apply: 'restart',
    control: 'number',
    unit: '个',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'usage_log_retention_days',
    group: 'logs',
    label: '日志保留天数',
    apply: 'restart',
    control: 'number',
    unit: '天',
    min: 1,
    max: MAX_SAFE_INTEGER
  },
  {
    key: 'admin_logs_page_size_max',
    group: 'logs',
    label: '管理日志单页上限',
    apply: 'immediate',
    control: 'number',
    unit: '条',
    min: 200,
    max: MAX_SAFE_INTEGER
  }
]

const normalizeProbeDelays = (values: number[]): number[] => {
  if (
    values.length === 0 ||
    values.some(
      value =>
        !Number.isSafeInteger(value) || value < 1 || value > MAX_PROBE_DELAY_MS
    )
  ) {
    throw new Error('探测延迟必须是 1 到 60000 之间的整数')
  }
  return [...new Set(values)].sort((left, right) => left - right)
}

export const parseProbeDelays = (input: string): number[] => {
  const parts = input
    .split(',')
    .map(part => part.trim())
    .filter(Boolean)
  if (parts.length === 0 || parts.some(part => !/^\d+$/.test(part))) {
    throw new Error('请输入逗号分隔的毫秒整数')
  }
  return normalizeProbeDelays(parts.map(Number))
}

export const formatProbeDelays = (values: number[]): string => values.join(', ')

export const cloneRuntimeSettings = (settings: RuntimeSettings): RuntimeSettings => ({
  ...settings,
  upstream_concurrency_probe_delays_ms: [
    ...settings.upstream_concurrency_probe_delays_ms
  ]
})

const settingValuesEqual = (
  left: RuntimeSettings[RuntimeSettingKey],
  right: RuntimeSettings[RuntimeSettingKey]
): boolean => {
  if (Array.isArray(left) && Array.isArray(right)) {
    return left.length === right.length && left.every((value, index) => value === right[index])
  }
  return left === right
}

export const isRuntimeSettingsDirty = (
  loaded: RuntimeSettings,
  edited: RuntimeSettings
): boolean =>
  runtimeSettingFields.some(
    field => !settingValuesEqual(loaded[field.key], edited[field.key])
  )

export const changedRestartFields = (
  loaded: RuntimeSettings,
  edited: RuntimeSettings
): RuntimeSettingKey[] =>
  runtimeSettingFields
    .filter(
      field =>
        field.apply === 'restart' &&
        !settingValuesEqual(loaded[field.key], edited[field.key])
    )
    .map(field => field.key)

export const validateRuntimeSettings = (
  settings: RuntimeSettings
): RuntimeSettingsValidationErrors => {
  const errors: RuntimeSettingsValidationErrors = {}

  for (const field of runtimeSettingFields) {
    const value = settings[field.key]
    if (field.control === 'text') {
      if (typeof value !== 'string') {
        errors[field.key] = '不能为空'
      } else if (value.trim().length === 0 && field.allowEmpty !== true) {
        errors[field.key] = '不能为空'
      } else if (field.maxLength !== undefined && [...value.trim()].length > field.maxLength) {
        errors[field.key] = `最多 ${field.maxLength} 个字符`
      }
      continue
    }
    if (field.control !== 'number') continue
    if (typeof value !== 'number' || !Number.isFinite(value)) {
      errors[field.key] = '请输入有效数字'
      continue
    }
    if (field.integer !== false && !Number.isSafeInteger(value)) {
      errors[field.key] = '请输入整数'
      continue
    }
    if (field.min !== undefined && value < field.min) {
      errors[field.key] = `不能小于 ${field.min}`
    } else if (field.max !== undefined && value > field.max) {
      errors[field.key] = `不能大于 ${field.max}`
    }
  }

  let probeDelays: number[] | undefined
  try {
    probeDelays = normalizeProbeDelays(settings.upstream_concurrency_probe_delays_ms)
  } catch (error) {
    errors.upstream_concurrency_probe_delays_ms =
      error instanceof Error ? error.message : '探测延迟无效'
  }

  if (
    settings.upstream_transient_route_cooldown_base_seconds >
    settings.upstream_transient_route_cooldown_max_seconds
  ) {
    errors.upstream_transient_route_cooldown_base_seconds = '不能超过最大冷却时间'
  }
  if (
    settings.upstream_stream_keepalive_interval_seconds >=
    settings.upstream_stream_idle_timeout_seconds
  ) {
    errors.upstream_stream_keepalive_interval_seconds = '必须短于流式空闲超时'
  }
  // An interval longer than the whole wait budget would let the queue time out
  // before it ever polled, silently disabling it (mirrors the backend).
  if (
    settings.upstream_account_queue_poll_interval_ms >
    settings.upstream_account_queue_max_wait_ms
  ) {
    errors.upstream_account_queue_poll_interval_ms = '不能超过排队最大等待'
  }
  // C2.x: stale-after must be >= 2x the local lease heartbeat interval (ttl/3),
  // otherwise a healthy-but-silent long request gets its lease reclaimed
  // before its own heartbeat (mirrors the backend validation).
  if (
    Number.isSafeInteger(settings.upstream_local_lease_ttl_seconds) &&
    settings.upstream_local_lease_ttl_seconds > 0 &&
    settings.upstream_lease_stale_after_ms <
      2 * ((settings.upstream_local_lease_ttl_seconds * 1_000) / 3)
  ) {
    errors.upstream_lease_stale_after_ms =
      '必须 ≥ 本地并发租约心跳间隔（ttl/3）的 2 倍，否则长请求租约会被误回收'
  }
  // T1.1: cooldown-ceiling invariant linkage hint.  The effective ceiling
  // (max of the upstream Retry-After cooldown cap and the local backoff curve
  // at max_step) must stay strictly below the retry wait budget, otherwise
  // GiveUpReason::WaitBudget fires before any inter-round wait.
  const effectiveStep = Math.max(
    1,
    settings.upstream_transient_route_cooldown_max_step || 3
  )
  const curveCeiling =
    (settings.upstream_transient_route_cooldown_base_seconds || 1) *
    Math.pow(2, effectiveStep - 1)
  const ceiling = Math.max(
    settings.upstream_retry_after_cooldown_cap_seconds || 5,
    curveCeiling
  )
  const hardMax = settings.upstream_transient_route_cooldown_max_seconds || 300
  const boundedCeiling = Math.min(ceiling, hardMax)
  if (boundedCeiling * 1000 >= (settings.upstream_route_exhaustion_retry_max_wait_ms || 30000)) {
    errors.upstream_transient_route_cooldown_max_step =
      `冷却上界 ${boundedCeiling}s×1000 ≥ 轮间等待预算 ${settings.upstream_route_exhaustion_retry_max_wait_ms}ms，会必现路由耗尽；请降低冷却相关参数或提高等待预算`
  }
  if (
    settings.upstream_stream_idle_timeout_seconds >
    settings.upstream_stream_max_duration_seconds
  ) {
    errors.upstream_stream_idle_timeout_seconds = '不能超过流式最大时长'
  }

  const minimumFirstSemantic =
    Math.ceil(settings.upstream_concurrency_recovery_max_wait_ms / 1_000) +
    settings.upstream_response_header_timeout_seconds +
    settings.upstream_stream_idle_timeout_seconds
  if (
    Number.isSafeInteger(minimumFirstSemantic) &&
    settings.upstream_first_semantic_output_timeout_seconds < minimumFirstSemantic
  ) {
    errors.upstream_first_semantic_output_timeout_seconds =
      `不能小于并发、响应头和空闲预算之和 ${minimumFirstSemantic} 秒`
  }

  if (
    settings.upstream_first_output_warn_after_seconds >
    settings.upstream_first_semantic_output_timeout_seconds
  ) {
    errors.upstream_first_output_warn_after_seconds = '不能超过首个语义输出超时'
  }

  if (
    probeDelays !== undefined &&
    Number.isSafeInteger(settings.upstream_concurrency_recovery_max_rounds) &&
    settings.upstream_concurrency_recovery_max_rounds > 0
  ) {
    let covered = 0
    for (let round = 1; round < settings.upstream_concurrency_recovery_max_rounds; round += 1) {
      covered += probeDelays[Math.min(round - 1, probeDelays.length - 1)]
    }
    if (covered < settings.upstream_concurrency_recovery_max_wait_ms) {
      errors.upstream_concurrency_recovery_max_rounds = '当前轮次无法覆盖并发恢复等待预算'
    }
  }

  return errors
}
