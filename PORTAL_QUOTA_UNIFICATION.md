# 门户配额显示统一修改

## 修改概述

将门户页面的"每分钟请求限制"统一改为"xxx小时，xxx次请求"的格式,与下游配置保持一致。

## 修改内容

### 1. 前端类型定义 (`frontend/src/types/index.ts`)

**移除**:
- `PerMinuteUsage` 接口(从 Portal 相关类型中移除)
- `PortalOverview.quota_summary.per_minute` 字段
- `PortalQuota.per_minute` 字段

**保留**:
- `RequestQuotaUsage` 接口(包含 `window_hours` 和 `limit` 字段)
- 所有 Portal 类型现在统一使用 `request_quota` 字段

### 2. 概览页面 (`frontend/src/views/portal/Overview.vue`)

**修改前**:
```vue
<el-statistic title="每分钟使用量">
  <template #suffix>/ {{ data.quota_summary.per_minute.limit }}</template>
  {{ data.quota_summary.per_minute.used }}
</el-statistic>
```

**修改后**:
```vue
<el-statistic :title="`请求配额 (${data.quota_summary.request_quota.window_hours}小时)`">
  <template #suffix>/ {{ data.quota_summary.request_quota.limit }}</template>
  {{ data.quota_summary.request_quota.used }}
</el-statistic>
```

**变化**:
- 移除"每分钟使用量"卡片
- 统一使用"请求配额 (X小时)"格式
- 显示滑动窗口的时间范围

### 3. 限额详情页面 (`frontend/src/views/portal/QuotaDetails.vue`)

**修改前**:
```vue
<div class="section">
  <h3>每分钟限制</h3>
  <el-descriptions>
    <el-descriptions-item label="当前使用">{{ data.per_minute.used }}</el-descriptions-item>
    <el-descriptions-item label="限制">{{ data.per_minute.limit }}</el-descriptions-item>
  </el-descriptions>
</div>
```

**修改后**:
```vue
<div class="section" v-if="data.request_quota">
  <h3>请求配额（滑动窗口）</h3>
  <el-descriptions>
    <el-descriptions-item label="时间窗口">{{ data.request_quota.window_hours }} 小时</el-descriptions-item>
    <el-descriptions-item label="配额限制">{{ data.request_quota.limit }}</el-descriptions-item>
    <el-descriptions-item label="已使用">{{ data.request_quota.used }}</el-descriptions-item>
    <el-descriptions-item label="剩余">{{ data.request_quota.remaining }}</el-descriptions-item>
  </el-descriptions>
</div>
```

**变化**:
- 移除"每分钟限制"部分
- 统一使用"请求配额（滑动窗口）"
- 显示更详细的信息:时间窗口、配额限制、已使用、剩余

## 统一后的显示格式

### 下游配置 (Admin)
```
请求配额: 1小时 / 100次请求
```

### 门户显示 (Portal)
```
请求配额 (1小时): 45 / 100
使用率: 45%
```

## 数据流

1. **下游配置** (`DownstreamConfig`)
   - `request_quota_window_hours`: 时间窗口(小时)
   - `request_quota_requests`: 请求次数限制

2. **后端计算** (`AppState::compute_request_quota_usage`)
   - 计算滑动窗口内的实际使用量
   - 返回 `RequestQuotaUsage` 结构

3. **前端显示** (`Portal` 页面)
   - 显示格式: "请求配额 (X小时)"
   - 显示使用量、限制、剩余、百分比

## 优势

1. **统一性**: 管理后台和门户页面使用相同的配额概念
2. **灵活性**: 支持任意时间窗口(1小时、5小时、24小时等)
3. **清晰性**: 明确显示时间窗口,避免混淆
4. **一致性**: 与下游配置的 `request_quota_window_hours` 字段对应

## 注意事项

- 如果下游没有配置 `request_quota`,门户页面会隐藏该部分
- 后端 API 仍然返回 `per_minute_limit` 数据(向后兼容),但前端不再显示
- 建议所有下游配置都使用 `request_quota` 而不是 `per_minute_limit`

## 后续建议

1. 考虑在后端完全移除 `per_minute_limit` 相关逻辑
2. 更新文档,说明推荐使用请求配额而不是每分钟限制
3. 提供迁移工具,将现有的 `per_minute_limit` 配置转换为 `request_quota`
