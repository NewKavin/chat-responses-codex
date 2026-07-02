# Admin Observability UI Optimization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Optimize the admin and portal observability pages so slow model calls, empty responses, gateway/upstream errors, log categories, and model probe health are visible without changing provider protocol conversion.

**Architecture:** Keep this as a frontend-first observability pass. Extract reusable display logic into TypeScript utilities, then let `Playground.vue`, `Logs.vue`, `Dashboard.vue`, and `ModelProbeBoard.vue` consume those utilities. Do not change Chat Completions, Responses, Claude, tool-call, routing, or upstream parameter-cleaning behavior.

**Tech Stack:** Vue 3, TypeScript, Element Plus, ECharts, Vitest, existing Rust/Axum gateway for deployment smoke tests.

---

## Scope Check

This plan covers three related UI surfaces from one approved spec: model playground, log diagnosis, and model health probing. They share the same operator workflow: reproduce a model issue, classify the error, and inspect channel health. The plan keeps them in one implementation sequence but splits code by responsibility so each task is independently testable.

The existing `/api/admin/logs` endpoint returns paginated logs, not an aggregate over the full filtered range. To avoid a backend query redesign in this UI pass, the log summary in this plan is explicitly labeled as a visible-page summary. A future backend aggregation field can replace it without changing the page layout.

## File Structure

- Create `frontend/src/utils/errorDisplay.ts`: shared parsing and summary helpers for API error bodies, long JSON errors, and user-facing error text.
- Create `frontend/tests/utils/errorDisplay.spec.ts`: unit tests for structured error extraction and truncation behavior.
- Modify `frontend/src/utils/logDisplay.ts`: error category metadata, category grouping, log summary counts, and category labels.
- Modify `frontend/tests/utils/logDisplay.spec.ts`: tests for category labels, visible-page summaries, and inference strength regressions.
- Modify `frontend/src/utils/modelProbeCharts.ts`: filtering, anomaly-first sorting, empty chart helpers, and status labels.
- Modify `frontend/tests/utils/modelProbeCharts.spec.ts`: tests for search, status filter, anomaly-first ordering, and empty data behavior.
- Modify `frontend/src/utils/playground.ts`: playground usage/meta formatting and readable error extraction hooks.
- Modify `frontend/tests/utils/playground.spec.ts`: tests for empty completion text, usage/meta formatting, stream status, and structured stream errors.
- Modify `frontend/src/components/ModelProbeBoard.vue`: search, status filter, anomaly sorting, better empty/error states, compact model lists, and chart empty states.
- Modify `frontend/src/views/admin/ModelProbe.vue`: pass explicit load error state into `ModelProbeBoard`.
- Modify `frontend/src/views/portal/ModelProbe.vue`: pass explicit load error state into `ModelProbeBoard`.
- Modify `frontend/src/views/admin/Dashboard.vue`: add a model health strip/card backed by `adminApi.getModelProbe()`, with navigation to admin model probe.
- Modify `frontend/src/views/admin/Logs.vue`: move category metadata to utilities, add visible-page summary, improve error display, empty state, and load-error message.
- Modify `frontend/src/views/portal/Playground.vue`: show richer progress/meta/error states using the new utilities.

---

### Task 1: Shared Display Utilities

**Files:**
- Create: `frontend/src/utils/errorDisplay.ts`
- Modify: `frontend/src/utils/logDisplay.ts`
- Modify: `frontend/src/utils/modelProbeCharts.ts`
- Modify: `frontend/src/utils/playground.ts`
- Test: `frontend/tests/utils/errorDisplay.spec.ts`
- Test: `frontend/tests/utils/logDisplay.spec.ts`
- Test: `frontend/tests/utils/modelProbeCharts.spec.ts`
- Test: `frontend/tests/utils/playground.spec.ts`

- [ ] **Step 1: Write failing tests for `errorDisplay`**

Create `frontend/tests/utils/errorDisplay.spec.ts`:

```ts
import { describe, expect, it } from 'vitest'
import {
  extractReadableErrorMessage,
  summarizeErrorText,
  type ReadableErrorSource
} from '../../src/utils/errorDisplay'

describe('errorDisplay', () => {
  it('extracts OpenAI-compatible error messages', () => {
    const source: ReadableErrorSource = {
      status: 503,
      statusText: 'Service Unavailable',
      body: {
        error: {
          message: 'upstream temporary unavailable',
          code: 'upstream_temporary_unavailable',
          category: 'upstream_temporary_unavailable'
        }
      }
    }

    expect(extractReadableErrorMessage(source)).toBe(
      '503 Service Unavailable：upstream temporary unavailable（upstream_temporary_unavailable）'
    )
  })

  it('extracts message from JSON text bodies', () => {
    expect(
      extractReadableErrorMessage({
        status: 429,
        statusText: 'Too Many Requests',
        bodyText: '{"error":{"message":"日 Token 限额已用尽","code":"gateway_daily_token_quota_exceeded"}}'
      })
    ).toBe('429 Too Many Requests：日 Token 限额已用尽（gateway_daily_token_quota_exceeded）')
  })

  it('summarizes long plain text without breaking short messages', () => {
    expect(summarizeErrorText('short message')).toBe('short message')
    expect(summarizeErrorText('x'.repeat(220), 24)).toBe(`${'x'.repeat(24)}...`)
    expect(summarizeErrorText('')).toBe('-')
  })
})
```

- [ ] **Step 2: Run red tests for `errorDisplay`**

Run:

```bash
cd frontend && rtk npx vitest run tests/utils/errorDisplay.spec.ts
```

Expected: fail because `frontend/src/utils/errorDisplay.ts` does not exist.

- [ ] **Step 3: Implement `errorDisplay`**

Create `frontend/src/utils/errorDisplay.ts`:

```ts
export interface ReadableErrorSource {
  status?: number
  statusText?: string
  body?: unknown
  bodyText?: string
  fallback?: string
}

interface StructuredErrorParts {
  message: string
  code?: string
  category?: string
  type?: string
}

const normalizeText = (value: unknown) =>
  typeof value === 'string' ? value.replace(/\s+/g, ' ').trim() : ''

const tryParseJson = (value?: string): unknown => {
  const text = value?.trim()
  if (!text || (!text.startsWith('{') && !text.startsWith('['))) return undefined
  try {
    return JSON.parse(text)
  } catch {
    return undefined
  }
}

const extractStructuredParts = (body: unknown): StructuredErrorParts | null => {
  if (!body || typeof body !== 'object') return null
  const objectBody = body as Record<string, unknown>
  const nested = objectBody.error
  if (typeof nested === 'string') {
    const message = normalizeText(nested)
    return message ? { message } : null
  }
  if (nested && typeof nested === 'object') {
    const error = nested as Record<string, unknown>
    const message = normalizeText(error.message)
    if (!message) return null
    return {
      message,
      code: normalizeText(error.code) || undefined,
      category: normalizeText(error.category) || normalizeText(objectBody.category) || undefined,
      type: normalizeText(error.type) || undefined
    }
  }
  const message = normalizeText(objectBody.message) || normalizeText(objectBody.detail)
  if (!message) return null
  return {
    message,
    code: normalizeText(objectBody.code) || undefined,
    category: normalizeText(objectBody.category) || undefined,
    type: normalizeText(objectBody.type) || undefined
  }
}

export const summarizeErrorText = (value?: string | null, maxLength = 180) => {
  const text = normalizeText(value)
  if (!text) return '-'
  if (text.length <= maxLength) return text
  return `${text.slice(0, maxLength)}...`
}

export const extractReadableErrorMessage = ({
  status,
  statusText,
  body,
  bodyText,
  fallback = '请求失败'
}: ReadableErrorSource) => {
  const parsedBody = body ?? tryParseJson(bodyText)
  const parts = extractStructuredParts(parsedBody)
  const plainText = summarizeErrorText(bodyText, 240)
  const message = parts?.message || (plainText !== '-' ? plainText : fallback)
  const detail = [...new Set([parts?.category, parts?.code, parts?.type].filter(Boolean))].join(' / ')
  const statusPrefix =
    typeof status === 'number'
      ? `${status}${statusText ? ` ${statusText}` : ''}：`
      : ''

  return `${statusPrefix}${message}${detail ? `（${detail}）` : ''}`
}
```

- [ ] **Step 4: Run green tests for `errorDisplay`**

Run:

```bash
cd frontend && rtk npx vitest run tests/utils/errorDisplay.spec.ts
```

Expected: pass.

- [ ] **Step 5: Write failing tests for log display helpers**

Extend `frontend/tests/utils/logDisplay.spec.ts`:

```ts
import {
  buildVisibleLogSummary,
  errorCategoryGroups,
  formatErrorCategory,
  formatInferenceStrength
} from '../../src/utils/logDisplay'

describe('error category display', () => {
  it('keeps gateway quota and upstream categories available', () => {
    const values = errorCategoryGroups.flatMap(group => group.options.map(option => option.value))
    expect(values).toContain('gateway_daily_token_quota_exceeded')
    expect(values).toContain('upstream_temporary_unavailable')
    expect(values).toContain('stream_idle_timeout')
  })

  it('formats known and unknown categories', () => {
    expect(formatErrorCategory('gateway_daily_token_quota_exceeded')).toBe('日 Token 限额')
    expect(formatErrorCategory('custom_error')).toBe('custom_error')
    expect(formatErrorCategory('')).toBe('-')
  })

  it('summarizes visible page failures by group', () => {
    const summary = buildVisibleLogSummary([
      { status_code: 200, error_category: '' },
      { status_code: 429, error_category: 'gateway_daily_token_quota_exceeded' },
      { status_code: 503, error_category: 'upstream_temporary_unavailable' },
      { status_code: 504, error_category: 'stream_idle_timeout' }
    ])

    expect(summary.total).toBe(4)
    expect(summary.failed).toBe(3)
    expect(summary.gatewayQuota).toBe(1)
    expect(summary.upstreamFeedback).toBe(1)
    expect(summary.streaming).toBe(1)
  })
})
```

- [ ] **Step 6: Run red tests for log display helpers**

Run:

```bash
cd frontend && rtk npx vitest run tests/utils/logDisplay.spec.ts
```

Expected: fail because the new helpers do not exist.

- [ ] **Step 7: Implement log display helpers**

Add to `frontend/src/utils/logDisplay.ts` while keeping `formatInferenceStrength`:

```ts
export interface ErrorCategoryOption {
  value: string
  label: string
}

export interface ErrorCategoryGroup {
  key: 'gateway_access' | 'gateway_quota' | 'upstream_feedback' | 'upstream_response' | 'streaming'
  label: string
  options: ErrorCategoryOption[]
}

export const errorCategoryGroups: ErrorCategoryGroup[] = [
  {
    key: 'gateway_access',
    label: '网关访问',
    options: [
      { value: 'gateway_auth_invalid', label: '认证无效' },
      { value: 'gateway_key_expired', label: 'Key 已过期' },
      { value: 'gateway_ip_not_allowed', label: 'IP 不允许' },
      { value: 'gateway_model_not_allowed', label: '模型不允许' },
      { value: 'gateway_no_routable_upstream', label: '无可路由上游' },
      { value: 'gateway_invalid_request', label: '请求无效' },
      { value: 'gateway_response_history_invalid', label: '响应历史无效' }
    ]
  },
  {
    key: 'gateway_quota',
    label: '网关配额',
    options: [
      { value: 'gateway_per_minute_limit_exceeded', label: '分钟请求限额' },
      { value: 'gateway_request_quota_exceeded', label: '窗口请求限额' },
      { value: 'gateway_daily_token_quota_exceeded', label: '日 Token 限额' },
      { value: 'gateway_monthly_token_quota_exceeded', label: '月 Token 限额' },
      { value: 'gateway_concurrency_full', label: '下游并发已满' }
    ]
  },
  {
    key: 'upstream_feedback',
    label: '上游反馈',
    options: [
      { value: 'upstream_auth_error', label: '上游认证错误' },
      { value: 'upstream_rate_limited', label: '上游限流' },
      { value: 'upstream_concurrency_full', label: '上游并发已满' },
      { value: 'upstream_protocol_unsupported', label: '上游协议不支持' },
      { value: 'upstream_context_limit', label: '上下文超限' },
      { value: 'upstream_request_rejected', label: '上游拒绝请求' },
      { value: 'upstream_temporary_unavailable', label: '上游临时不可用' }
    ]
  },
  {
    key: 'upstream_response',
    label: '上游响应',
    options: [
      { value: 'upstream_timeout', label: '上游超时' },
      { value: 'upstream_network_error', label: '上游网络错误' },
      { value: 'upstream_invalid_response', label: '上游响应无效' },
      { value: 'upstream_empty_response', label: '上游空响应' }
    ]
  },
  {
    key: 'streaming',
    label: '流式中断',
    options: [
      { value: 'stream_client_cancelled', label: '客户端取消，无输出' },
      { value: 'stream_incomplete_close', label: '下游断连，已有部分输出' },
      { value: 'stream_interrupted', label: '下游断连，未分类' },
      { value: 'stream_upstream_body_decode_error', label: '上游响应解码失败' },
      { value: 'stream_upstream_read_error', label: '上游流读取失败' },
      { value: 'stream_upstream_timeout', label: '上游流超时' },
      { value: 'stream_idle_timeout', label: '空闲超时' },
      { value: 'stream_max_duration', label: '最大时长' }
    ]
  }
]

const categoryLookup = new Map(
  errorCategoryGroups.flatMap(group =>
    group.options.map(option => [option.value, { group: group.key, label: option.label }] as const)
  )
)

export const formatErrorCategory = (value?: string | null) => {
  const trimmed = value?.trim()
  if (!trimmed) return '-'
  return categoryLookup.get(trimmed)?.label ?? trimmed
}

export const buildVisibleLogSummary = (
  logs: Array<{ status_code: number; error_category?: string | null }>
) => {
  const summary = {
    total: logs.length,
    failed: 0,
    gatewayAccess: 0,
    gatewayQuota: 0,
    upstreamFeedback: 0,
    upstreamResponse: 0,
    streaming: 0,
    uncategorized: 0
  }

  for (const log of logs) {
    if (log.status_code < 400 && !log.error_category) continue
    summary.failed += 1
    const category = categoryLookup.get(log.error_category?.trim() || '')
    if (!category) {
      summary.uncategorized += 1
      continue
    }
    if (category.group === 'gateway_access') summary.gatewayAccess += 1
    if (category.group === 'gateway_quota') summary.gatewayQuota += 1
    if (category.group === 'upstream_feedback') summary.upstreamFeedback += 1
    if (category.group === 'upstream_response') summary.upstreamResponse += 1
    if (category.group === 'streaming') summary.streaming += 1
  }

  return summary
}
```

- [ ] **Step 8: Write failing tests for model probe helpers**

Extend `frontend/tests/utils/modelProbeCharts.spec.ts`:

```ts
import {
  buildProbeChartItems,
  filterProbeChannels,
  sortProbeChannels,
  type ProbeChannelFilter
} from '../../src/utils/modelProbeCharts'

describe('model probe filtering', () => {
  const channels = [
    {
      upstream_id: 'up-1',
      upstream_name: 'Alpha',
      key_prefix: 'ak-alpha',
      status: 'healthy',
      latency_ms: 100,
      model_count: 2,
      models: ['glm-5.1', 'deepseek-chat'],
      last_probe_at: 1,
      error: null
    },
    {
      upstream_id: 'up-2',
      upstream_name: 'Beta',
      key_prefix: 'bk-beta',
      status: 'offline',
      latency_ms: 0,
      model_count: 0,
      models: [],
      last_probe_at: 1,
      error: 'timeout'
    }
  ]

  it('filters channels by text and status', () => {
    const filter: ProbeChannelFilter = { query: 'glm', status: 'healthy' }
    expect(filterProbeChannels(channels, filter).map(channel => channel.upstream_name)).toEqual(['Alpha'])
    expect(filterProbeChannels(channels, { query: 'beta', status: 'offline' }).map(channel => channel.upstream_name)).toEqual(['Beta'])
  })

  it('sorts anomalies first when requested', () => {
    expect(sortProbeChannels(channels, { anomalyFirst: true }).map(channel => channel.upstream_name)).toEqual(['Beta', 'Alpha'])
  })

  it('returns no chart items for empty status data', () => {
    expect(buildProbeChartItems({ healthy_channels: 0, degraded_channels: 0, offline_channels: 0 })).toEqual([])
  })
})
```

- [ ] **Step 9: Run red tests for model probe helpers**

Run:

```bash
cd frontend && rtk npx vitest run tests/utils/modelProbeCharts.spec.ts
```

Expected: fail because helper types/functions do not exist and `sortProbeChannels` lacks options.

- [ ] **Step 10: Implement model probe helpers**

Update `frontend/src/utils/modelProbeCharts.ts`:

```ts
export type ProbeStatusFilter = 'all' | 'healthy' | 'degraded' | 'offline'

export interface ProbeChannelFilter {
  query: string
  status: ProbeStatusFilter
}

const normalStatusRank = (status: ModelProbeChannel['status']) => {
  if (status === 'healthy') return 0
  if (status === 'degraded') return 1
  return 2
}

const anomalyStatusRank = (status: ModelProbeChannel['status']) => {
  if (status === 'offline') return 0
  if (status === 'degraded') return 1
  if (status === 'healthy') return 2
  return 3
}

export const sortProbeChannels = (
  channels: ModelProbeChannel[],
  options: { anomalyFirst?: boolean } = {}
) =>
  [...channels].sort((left, right) => {
    const rank = options.anomalyFirst ? anomalyStatusRank : normalStatusRank
    const statusOrder = rank(left.status) - rank(right.status)
    if (statusOrder !== 0) return statusOrder
    return (
      left.upstream_name.localeCompare(right.upstream_name) ||
      left.key_prefix.localeCompare(right.key_prefix)
    )
  })

export const filterProbeChannels = (
  channels: ModelProbeChannel[],
  filter: ProbeChannelFilter
) => {
  const query = filter.query.trim().toLowerCase()
  return channels.filter(channel => {
    const statusMatch = filter.status === 'all' || channel.status === filter.status
    if (!statusMatch) return false
    if (!query) return true
    return [
      channel.upstream_name,
      channel.key_prefix,
      channel.error ?? '',
      ...channel.models
    ].some(value => value.toLowerCase().includes(query))
  })
}

export const buildProbeChartItems = (summary: {
  healthy_channels: number
  degraded_channels: number
  offline_channels: number
}) =>
  [
    { name: '健康', value: summary.healthy_channels },
    { name: '降级', value: summary.degraded_channels },
    { name: '离线', value: summary.offline_channels }
  ].filter(item => item.value > 0)
```

Keep `sortProbeModels`, `groupTopProbeModels`, and `formatProbeStatusLabel` exports. Remove the old private `statusRank` helper after replacing it.

- [ ] **Step 11: Write failing tests for playground display helpers**

Extend `frontend/tests/utils/playground.spec.ts`:

```ts
import {
  formatPlaygroundCompletionMeta,
  formatPlaygroundUsageText
} from '../../src/utils/playground'

describe('playground display helpers', () => {
  it('formats usage and elapsed metadata', () => {
    expect(
      formatPlaygroundUsageText({
        prompt_tokens: 10,
        completion_tokens: 20,
        total_tokens: 30
      })
    ).toBe('tokens: in=10 out=20 total=30')

    expect(
      formatPlaygroundCompletionMeta({
        model: 'glm-5.1',
        elapsedSeconds: 12,
        firstOutputSeconds: 5,
        usageText: 'tokens: in=10 out=20 total=30'
      })
    ).toBe('模型 glm-5.1 · 总耗时 12s · 首包 5s · tokens: in=10 out=20 total=30')
  })
})
```

- [ ] **Step 12: Run red tests for playground display helpers**

Run:

```bash
cd frontend && rtk npx vitest run tests/utils/playground.spec.ts
```

Expected: fail because the new helper exports do not exist.

- [ ] **Step 13: Implement playground display helpers**

Add to `frontend/src/utils/playground.ts`:

```ts
type UsageLike = {
  prompt_tokens: number
  completion_tokens: number
  total_tokens: number
} | null | undefined

export const formatPlaygroundUsageText = (usage: UsageLike) => {
  if (!usage) return undefined
  return `tokens: in=${usage.prompt_tokens} out=${usage.completion_tokens} total=${usage.total_tokens}`
}

export const formatPlaygroundCompletionMeta = ({
  model,
  elapsedSeconds,
  firstOutputSeconds,
  usageText
}: {
  model: string
  elapsedSeconds: number
  firstOutputSeconds?: number
  usageText?: string
}) => {
  const parts = [`模型 ${model}`, `总耗时 ${Math.max(0, Math.floor(elapsedSeconds))}s`]
  if (typeof firstOutputSeconds === 'number') {
    parts.push(`首包 ${Math.max(0, Math.floor(firstOutputSeconds))}s`)
  }
  if (usageText) {
    parts.push(usageText)
  }
  return parts.join(' · ')
}
```

- [ ] **Step 14: Run all focused utility tests**

Run:

```bash
cd frontend && rtk npx vitest run tests/utils/errorDisplay.spec.ts tests/utils/logDisplay.spec.ts tests/utils/modelProbeCharts.spec.ts tests/utils/playground.spec.ts
```

Expected: all focused utility tests pass.

- [ ] **Step 15: Commit shared utilities**

Run:

```bash
rtk git add frontend/src/utils/errorDisplay.ts frontend/src/utils/logDisplay.ts frontend/src/utils/modelProbeCharts.ts frontend/src/utils/playground.ts frontend/tests/utils/errorDisplay.spec.ts frontend/tests/utils/logDisplay.spec.ts frontend/tests/utils/modelProbeCharts.spec.ts frontend/tests/utils/playground.spec.ts
rtk git commit -m "feat: add observability display utilities"
```

Expected: commit succeeds.

---

### Task 2: Model Probe And Dashboard Health UI

**Files:**
- Modify: `frontend/src/components/ModelProbeBoard.vue`
- Modify: `frontend/src/views/admin/ModelProbe.vue`
- Modify: `frontend/src/views/portal/ModelProbe.vue`
- Modify: `frontend/src/views/admin/Dashboard.vue`
- Test: `frontend/tests/utils/modelProbeCharts.spec.ts`

- [ ] **Step 1: Update `ModelProbeBoard` props and state**

Modify `frontend/src/components/ModelProbeBoard.vue` props:

```ts
const props = defineProps<{
  tone: 'admin' | 'portal'
  scopeLabel: string
  title: string
  subtitle: string
  data: ModelProbeResponse
  loading?: boolean
  errorMessage?: string
}>()
```

Add local controls:

```ts
const searchQuery = ref('')
const statusFilter = ref<ProbeStatusFilter>('all')
const anomalyFirst = ref(true)
```

Update imports:

```ts
import {
  buildProbeChartItems,
  filterProbeChannels,
  formatProbeStatusLabel,
  groupTopProbeModels,
  sortProbeChannels,
  type ProbeStatusFilter
} from '@/utils/modelProbeCharts'
```

- [ ] **Step 2: Fix empty/error state calculations**

Replace current computed values:

```ts
const hasError = computed(() => Boolean(props.errorMessage && !loading.value))
const isEmpty = computed(() => !hasError.value && props.data.summary?.total_channels === 0 && !loading.value)
const errorMessage = computed(() => props.errorMessage || '模型探测失败，请检查上游配置或稍后重试')
```

Replace sorted channels:

```ts
const filteredChannels = computed(() =>
  filterProbeChannels(props.data.channels, {
    query: searchQuery.value,
    status: statusFilter.value
  })
)

const sortedChannels = computed(() =>
  sortProbeChannels(filteredChannels.value, { anomalyFirst: anomalyFirst.value })
)
```

- [ ] **Step 3: Add toolbar above channel details**

In the `通道状态明细` card header area, add a compact toolbar before `.channel-grid`:

```vue
<div class="probe-toolbar">
  <el-input
    v-model="searchQuery"
    clearable
    placeholder="搜索上游、Key 前缀或模型"
    class="probe-toolbar__search"
  />
  <el-segmented
    v-model="statusFilter"
    :options="[
      { label: '全部', value: 'all' },
      { label: '健康', value: 'healthy' },
      { label: '降级', value: 'degraded' },
      { label: '离线', value: 'offline' }
    ]"
  />
  <el-switch v-model="anomalyFirst" active-text="异常优先" inactive-text="默认排序" />
</div>
```

Update the card count tag to use `sortedChannels.length`.

- [ ] **Step 4: Improve channel list empty state**

Inside `.channel-grid`, add:

```vue
<div v-if="!sortedChannels.length && !loading" class="channel-empty">
  当前条件下暂无通道
</div>
```

Keep the existing `article v-for` after this block.

- [ ] **Step 5: Use real empty chart data**

Change `buildStatusSeries` to:

```ts
const buildStatusSeries = () => buildProbeChartItems(summary.value)
```

Change `buildCoverageSeries` to:

```ts
const buildCoverageSeries = () =>
  modelCoverage.value.items.map(item => ({
    name: item.model,
    value: item.channel_count
  }))
```

When setting status and coverage chart options, set a `graphic` empty label when the series array is empty, and pass that empty array to `series.data`. Do not use `{ name: '暂无数据', value: 1 }` or `{ name: '暂无模型', value: 1 }`.

- [ ] **Step 6: Limit long model lists**

Change `.channel-card__models-list` CSS:

```css
.channel-card__models-list {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  max-height: 96px;
  overflow-y: auto;
  padding-right: 4px;
}
```

Add toolbar CSS:

```css
.probe-toolbar {
  display: flex;
  flex-wrap: wrap;
  gap: 12px;
  align-items: center;
  margin-bottom: 16px;
}

.probe-toolbar__search {
  max-width: 320px;
  min-width: 220px;
}

.channel-empty {
  grid-column: 1 / -1;
  padding: 28px;
  text-align: center;
  color: #64748b;
  border: 1px dashed rgba(148, 163, 184, 0.38);
  border-radius: 14px;
  background: #f8fafc;
}
```

- [ ] **Step 7: Pass explicit load errors from model probe pages**

In both `frontend/src/views/admin/ModelProbe.vue` and `frontend/src/views/portal/ModelProbe.vue`, add:

```ts
const loadError = ref('')
```

Pass it:

```vue
:error-message="loadError"
```

In `loadData`, clear before request and set on catch:

```ts
loadError.value = ''
const errorMsg = error?.response?.data?.error?.message || '加载模型探测失败'
loadError.value = errorMsg
ElMessage.error(errorMsg)
```

- [ ] **Step 8: Add Dashboard model health state**

In `frontend/src/views/admin/Dashboard.vue`, import router and model probe types:

```ts
import { useRouter } from 'vue-router'
import type { DashboardAnalyticsRange, DashboardBreakdownItem, DashboardData, ModelProbeResponse } from '@/types'
import { DEFAULT_MODEL_PROBE_REFRESH_INTERVAL_SECONDS } from '@/utils/modelProbePolling'
```

Add state:

```ts
const router = useRouter()
const modelProbeLoading = ref(false)
const modelProbeError = ref('')
const modelProbe = ref<ModelProbeResponse>({
  refreshed_at: 0,
  refresh_interval_seconds: DEFAULT_MODEL_PROBE_REFRESH_INTERVAL_SECONDS,
  summary: {
    total_channels: 0,
    healthy_channels: 0,
    offline_channels: 0,
    degraded_channels: 0,
    total_models: 0,
    average_latency_ms: 0
  },
  channels: [],
  models: []
})

const modelProbeRefreshedLabel = computed(() => {
  if (!modelProbe.value.refreshed_at) return '等待刷新'
  return new Date(modelProbe.value.refreshed_at * 1000).toLocaleString('zh-CN', {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit'
  })
})
```

Add loaders:

```ts
const loadModelProbe = async () => {
  try {
    modelProbeLoading.value = true
    modelProbeError.value = ''
    const { data } = await adminApi.getModelProbe()
    modelProbe.value = data
  } catch (error: any) {
    modelProbeError.value = error?.response?.data?.error?.message || '模型健康加载失败'
  } finally {
    modelProbeLoading.value = false
  }
}

const openModelProbe = () => {
  void router.push('/admin/model-probe')
}
```

Call it from `loadDashboard` after the dashboard response is applied and charts render, without blocking the dashboard:

```ts
void loadModelProbe()
```

- [ ] **Step 9: Add Dashboard model health block**

Insert after `.status-strip`:

```vue
<el-card shadow="hover" class="chart-card model-health-card" v-loading="modelProbeLoading">
  <template #header>
    <div class="card-header card-header--trend">
      <div>
        <h2>模型健康</h2>
        <p>来自模型探测的通道健康快照。</p>
      </div>
      <el-button size="small" type="primary" @click="openModelProbe">查看探测</el-button>
    </div>
  </template>
  <el-alert
    v-if="modelProbeError"
    :title="modelProbeError"
    type="warning"
    show-icon
    :closable="false"
    class="model-health-alert"
  />
  <div class="chart-summary model-health-summary">
    <div class="summary-chip">
      <strong>{{ modelProbe.summary.healthy_channels }}</strong>
      <span>健康通道</span>
    </div>
    <div class="summary-chip">
      <strong>{{ modelProbe.summary.degraded_channels }}</strong>
      <span>降级通道</span>
    </div>
    <div class="summary-chip">
      <strong>{{ modelProbe.summary.offline_channels }}</strong>
      <span>离线通道</span>
    </div>
    <div class="summary-chip">
      <strong>{{ modelProbe.summary.average_latency_ms }}ms</strong>
      <span>平均探测耗时</span>
    </div>
    <div class="summary-chip">
      <strong>{{ modelProbeRefreshedLabel }}</strong>
      <span>最近探测</span>
    </div>
  </div>
</el-card>
```

Add CSS:

```css
.model-health-card {
  margin-top: 18px;
}

.model-health-alert {
  margin-bottom: 12px;
}

.model-health-summary {
  margin-bottom: 0;
}
```

- [ ] **Step 10: Run focused model probe utility tests and build**

Run:

```bash
cd frontend && rtk npx vitest run tests/utils/modelProbeCharts.spec.ts
cd frontend && rtk npm run build
```

Expected: tests pass and frontend build succeeds.

- [ ] **Step 11: Commit model health UI**

Run:

```bash
rtk git add frontend/src/components/ModelProbeBoard.vue frontend/src/views/admin/ModelProbe.vue frontend/src/views/portal/ModelProbe.vue frontend/src/views/admin/Dashboard.vue frontend/src/utils/modelProbeCharts.ts frontend/tests/utils/modelProbeCharts.spec.ts
rtk git commit -m "feat: improve model probe health UI"
```

Expected: commit succeeds.

---

### Task 3: Logs Diagnosis UI

**Files:**
- Modify: `frontend/src/views/admin/Logs.vue`
- Modify: `frontend/src/utils/logDisplay.ts`
- Test: `frontend/tests/utils/logDisplay.spec.ts`

- [ ] **Step 1: Move category constants to utility imports**

In `frontend/src/views/admin/Logs.vue`, replace the local `errorCategoryGroups` constant with imports:

```ts
import {
  buildVisibleLogSummary,
  errorCategoryGroups,
  formatErrorCategory,
  formatInferenceStrength
} from '@/utils/logDisplay'
import { summarizeErrorText } from '@/utils/errorDisplay'
```

Keep `statusCodeOptions` local.

- [ ] **Step 2: Add display fields to `DisplayLog`**

Extend the interface:

```ts
interface DisplayLog extends UsageLog {
  apiName: string
  apiIcon: Component
  logType: string
  inferenceStrength: string
  billingMode: string
  requestCount: number
  userAgent: string
  downstreamName: string
  upstreamName: string
  errorCategoryLabel: string
  errorSummary: string
}
```

Add to `buildDisplayLog` return:

```ts
errorCategoryLabel: formatErrorCategory(log.error_category),
errorSummary: summarizeErrorText(log.error_message, 160)
```

- [ ] **Step 3: Add visible-page summary computed**

Add:

```ts
const visibleSummary = computed(() => buildVisibleLogSummary(logs.value))
```

- [ ] **Step 4: Render summary strip above the table**

Add after the helper alert:

```vue
<div class="log-summary-strip">
  <div class="log-summary-item">
    <span>当前页日志</span>
    <strong>{{ visibleSummary.total }}</strong>
  </div>
  <div class="log-summary-item log-summary-item--danger">
    <span>失败</span>
    <strong>{{ visibleSummary.failed }}</strong>
  </div>
  <div class="log-summary-item">
    <span>网关配额</span>
    <strong>{{ visibleSummary.gatewayQuota }}</strong>
  </div>
  <div class="log-summary-item">
    <span>上游反馈</span>
    <strong>{{ visibleSummary.upstreamFeedback }}</strong>
  </div>
  <div class="log-summary-item">
    <span>流式中断</span>
    <strong>{{ visibleSummary.streaming }}</strong>
  </div>
</div>
```

- [ ] **Step 5: Improve category and error columns**

Change the error category column body:

```vue
<el-tag v-if="row.error_category" size="small" type="danger" effect="plain">
  {{ row.errorCategoryLabel }}
</el-tag>
<span v-else>-</span>
```

Change the error message column body:

```vue
<el-tooltip
  v-if="row.error_message?.trim()"
  :content="row.error_message"
  placement="top"
  :show-after="300"
>
  <span>{{ row.errorSummary }}</span>
</el-tooltip>
<span v-else>-</span>
```

- [ ] **Step 6: Add empty table text**

Update table:

```vue
<el-table :data="tableRows" v-loading="loading" stripe empty-text="当前筛选条件下暂无日志">
```

- [ ] **Step 7: Surface backend load errors**

In `loadData` catch:

```ts
const errorMsg =
  (error as any)?.response?.data?.error?.message ||
  (error as any)?.response?.data?.message ||
  '加载日志失败'
ElMessage.error(errorMsg)
```

- [ ] **Step 8: Add summary CSS**

Add to scoped style:

```css
.log-summary-strip {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(128px, 1fr));
  gap: 10px;
  margin: 14px 0 16px;
}

.log-summary-item {
  padding: 12px 14px;
  border: 1px solid #e4e7ed;
  border-radius: 8px;
  background: #f8fafc;
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
}

.log-summary-item span {
  color: #606266;
  font-size: 12px;
}

.log-summary-item strong {
  color: #303133;
  font-size: 18px;
}

.log-summary-item--danger strong {
  color: #f56c6c;
}
```

- [ ] **Step 9: Run focused tests and build**

Run:

```bash
cd frontend && rtk npx vitest run tests/utils/logDisplay.spec.ts tests/utils/errorDisplay.spec.ts
cd frontend && rtk npm run build
```

Expected: tests pass and frontend build succeeds.

- [ ] **Step 10: Commit logs UI**

Run:

```bash
rtk git add frontend/src/views/admin/Logs.vue frontend/src/utils/logDisplay.ts frontend/src/utils/errorDisplay.ts frontend/tests/utils/logDisplay.spec.ts frontend/tests/utils/errorDisplay.spec.ts
rtk git commit -m "feat: improve admin log diagnosis UI"
```

Expected: commit succeeds.

---

### Task 4: Playground Slow/Empty/Error Feedback

**Files:**
- Modify: `frontend/src/views/portal/Playground.vue`
- Modify: `frontend/src/utils/playground.ts`
- Modify: `frontend/src/utils/errorDisplay.ts`
- Test: `frontend/tests/utils/playground.spec.ts`
- Test: `frontend/tests/utils/errorDisplay.spec.ts`

- [ ] **Step 1: Import display helpers**

In `frontend/src/views/portal/Playground.vue`, update imports:

```ts
import { extractReadableErrorMessage } from '@/utils/errorDisplay'
import {
  buildPlaygroundChatPayload,
  extractChatCompletionText,
  extractChatCompletionUsage,
  formatPlaygroundCompletionMeta,
  formatPlaygroundStreamStatus,
  formatPlaygroundUsageText,
  inferenceStrengthOptions,
  parseGatewayModels,
  parseSSELine,
  type PlaygroundMessage,
  type PlaygroundStreamPhase,
  type UploadedFileContext
} from '@/utils/playground'
```

- [ ] **Step 2: Extend conversation message metadata**

Update interface:

```ts
interface ConversationMessage {
  role: 'user' | 'assistant'
  content: string
  uploadedFiles?: UploadedFileContext[]
  usageText?: string
  reasoning?: string
  isError?: boolean
  isEmptyResponse?: boolean
}
```

- [ ] **Step 3: Track first output time**

Add refs:

```ts
const firstOutputSeconds = ref<number | undefined>(undefined)
```

In `startStreamTimer`, reset:

```ts
firstOutputSeconds.value = undefined
```

Add helper:

```ts
const markFirstOutput = () => {
  if (firstOutputSeconds.value !== undefined) return
  firstOutputSeconds.value = Math.max(0, Math.floor((Date.now() - streamStartedAt) / 1000))
}
```

Call `markFirstOutput()` before appending `chunk.reasoningContent` or `chunk.content`.

- [ ] **Step 4: Replace local usage formatter**

Remove local `formatUsage` and use `formatPlaygroundUsageText(finalUsage)`.

- [ ] **Step 5: Replace `safeGetText` with structured readable errors**

Update `safeGetText`:

```ts
const safeGetText = async (response: Response) => {
  const text = await response.text()
  return extractReadableErrorMessage({
    status: response.status,
    statusText: response.statusText,
    bodyText: text,
    fallback: `${response.status} ${response.statusText}`
  })
}
```

Keep the function name so the rest of the component changes stay small.

- [ ] **Step 6: Set clear empty-response messages**

After streaming or JSON response parsing:

```ts
const usageText = formatPlaygroundUsageText(finalUsage)
const elapsed = streamElapsedSeconds.value
const meta = formatPlaygroundCompletionMeta({
  model: selectedModel.value,
  elapsedSeconds: elapsed,
  firstOutputSeconds: firstOutputSeconds.value,
  usageText
})
const isEmptyResponse = !finalContent.trim()
const content =
  finalContent.trim() ||
  (finalReasoning ? '（模型仅返回思考过程，未返回正文）' : '（模型返回空内容）')
```

Push:

```ts
messages.value.push({
  role: 'assistant',
  content,
  reasoning: finalReasoning || undefined,
  usageText: meta,
  isEmptyResponse
})
```

- [ ] **Step 7: Render empty responses distinctly**

Update the message class binding:

```vue
:class="[
  'chat-message',
  `chat-message--${message.role}`,
  message.isError ? 'chat-message--error' : '',
  message.isEmptyResponse ? 'chat-message--empty-response' : ''
]"
```

Add CSS:

```css
.chat-message--empty-response .chat-message-content {
  border-color: #f3d19e;
  background: #fdf6ec;
  color: #b88230;
}
```

- [ ] **Step 8: Improve long wait status text**

In `frontend/src/utils/playground.ts`, adjust `formatPlaygroundStreamStatus`:

```ts
if (phase === 'waiting' || keepaliveCount > 0) {
  return seconds >= 30
    ? `模型仍在处理，已等待首个输出 ${seconds}s`
    : `已连接，等待模型首个输出 ${seconds}s`
}
return `正在连接模型 ${seconds}s`
```

Add this test:

```ts
expect(
  formatPlaygroundStreamStatus({
    phase: 'waiting',
    elapsedSeconds: 31,
    keepaliveCount: 2
  })
).toBe('模型仍在处理，已等待首个输出 31s')
```

- [ ] **Step 9: Clear first-output state**

In `clearConversation`, add:

```ts
firstOutputSeconds.value = undefined
```

In the catch path, keep upload restoration and clear streaming state as today.

- [ ] **Step 10: Run focused tests and build**

Run:

```bash
cd frontend && rtk npx vitest run tests/utils/playground.spec.ts tests/utils/errorDisplay.spec.ts
cd frontend && rtk npm run build
```

Expected: tests pass and frontend build succeeds.

- [ ] **Step 11: Commit playground UI**

Run:

```bash
rtk git add frontend/src/views/portal/Playground.vue frontend/src/utils/playground.ts frontend/src/utils/errorDisplay.ts frontend/tests/utils/playground.spec.ts frontend/tests/utils/errorDisplay.spec.ts
rtk git commit -m "feat: improve playground response feedback"
```

Expected: commit succeeds.

---

### Task 5: Full Verification And Deployment Smoke

**Files:**
- No source files required.

- [ ] **Step 1: Run all frontend utility tests**

Run:

```bash
cd frontend && rtk npx vitest run
```

Expected: all frontend Vitest specs pass.

- [ ] **Step 2: Build frontend**

Run:

```bash
cd frontend && rtk npm run build
```

Expected: `vue-tsc` and `vite build` succeed.

- [ ] **Step 3: Run backend tests**

Run:

```bash
rtk cargo test
```

Expected: Rust test suite passes. If an unrelated flaky integration test fails, rerun the failing test once and record the exact failure before changing code.

- [ ] **Step 4: Deploy to local docker directory**

Run:

```bash
cd ~/docker/chat-responses-codex && rtk docker compose build gateway
cd ~/docker/chat-responses-codex && rtk docker compose up -d --no-deps gateway
```

Expected: gateway container rebuilds and starts.

- [ ] **Step 5: Smoke health and frontend**

Run:

```bash
rtk curl -fsS http://127.0.0.1:3000/healthz
rtk curl -fsS http://127.0.0.1:3000/
```

Expected: health endpoint returns success and frontend HTML loads.

- [ ] **Step 6: Smoke model list**

Run:

```bash
rtk curl -fsS http://127.0.0.1:3000/v1/models -H 'Authorization: Bearer key-XVhmAgpudvd6rgbstasHXiPn3g5JaoWO'
```

Expected: JSON contains `data` with model ids.

- [ ] **Step 7: Smoke Chat Completions stream**

Run:

```bash
rtk curl -N -fsS http://127.0.0.1:3000/v1/chat/completions \
  -H 'Authorization: Bearer key-XVhmAgpudvd6rgbstasHXiPn3g5JaoWO' \
  -H 'Content-Type: application/json' \
  -d '{"model":"GLM-5.1","messages":[{"role":"user","content":"用一句话回复：网关连通性测试"}],"stream":true,"max_tokens":64}'
```

Expected: response streams SSE data or a readable structured upstream error. A readable quota/upstream error is acceptable for deployment diagnosis; a blank hang is not acceptable.

- [ ] **Step 8: Smoke Responses and Claude-compatible endpoints**

Run:

```bash
rtk curl -N -fsS http://127.0.0.1:3000/v1/responses \
  -H 'Authorization: Bearer key-XVhmAgpudvd6rgbstasHXiPn3g5JaoWO' \
  -H 'Content-Type: application/json' \
  -d '{"model":"GLM-5.1","input":"用一句话回复：responses 连通性测试","stream":true,"max_output_tokens":64}'

rtk curl -N -fsS http://127.0.0.1:3000/v1/messages \
  -H 'x-api-key: key-XVhmAgpudvd6rgbstasHXiPn3g5JaoWO' \
  -H 'anthropic-version: 2023-06-01' \
  -H 'Content-Type: application/json' \
  -d '{"model":"GLM-5.1","messages":[{"role":"user","content":"用一句话回复：claude messages 连通性测试"}],"stream":true,"max_tokens":64}'
```

Expected: both endpoints stream data or return readable structured errors without breaking gateway health.

- [ ] **Step 9: Verify admin pages manually**

Open:

```text
http://127.0.0.1:3000/#/admin/dashboard
http://127.0.0.1:3000/#/admin/logs
http://127.0.0.1:3000/#/admin/model-probe
```

Expected:
- Dashboard shows the model health block without overlapping existing charts.
- Logs page shows visible-page summary, category filters, empty state, and shortened errors.
- Model probe page allows search, status filtering, anomaly-first sorting, and clear empty/error states.

- [ ] **Step 10: Verify portal playground manually**

Open:

```text
http://127.0.0.1:3000/#/portal/playground
```

Expected:
- Slow stream shows connection/waiting/generating status.
- Empty model response shows “模型返回空内容” instead of looking stuck.
- Structured upstream errors show readable messages in the chat area.
