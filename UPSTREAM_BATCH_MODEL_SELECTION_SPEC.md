# 上游模型批量选择功能 - 技术方案

## 📋 功能概述

为"全局获取模型"功能增加批量选择能力，让用户可以：
1. 一键全选/反选模型
2. 使用预设模型组快速选择（GPT系列、Claude系列等）
3. 通过正则表达式筛选模型
4. 查看推荐模型（覆盖率>80%的模型）
5. 按支持账号数量排序

**效率提升：** 从手动勾选 23 次 → 点击 1 次预设按钮

## 🎯 用户故事

**改进前：**
> 用户点击"全局获取模型"，看到 156 个模型列表。
> 想启用所有 GPT 模型，需要在列表中找到并手动勾选每一个 gpt-* 模型。
> 如果有 20 个 GPT 模型，需要点击 20 次。

**改进后：**
> 用户点击"全局获取模型"，看到 156 个模型列表。
> 点击"GPT系列"按钮，自动选中所有 20 个 GPT 模型。
> 一键完成。

## 🏗️ 技术架构

### 1. 前端数据结构

```typescript
// 模型组定义
interface ModelGroup {
  id: string              // 唯一标识：'mainstream', 'gpt-series', 'claude-series'
  name: string            // 显示名称：'主流模型', 'GPT系列'
  description: string     // 描述：用于 tooltip
  pattern?: string        // 正则模式（动态组）：'^gpt-'
  models?: string[]       // 固定模型列表（静态组）
  builtin: boolean        // 是否为内置组
}

// 全局获取状态扩展
interface GlobalFetchState {
  visible: boolean
  fetching: boolean
  applying: boolean
  progress: { done: number; total: number }
  selectedModels: string[]
  upstreamModels: Record<string, { name: string; models: string[] }>
  
  // 新增字段
  filterText: string                // 筛选文本（支持正则）
  sortBy: 'name' | 'support_count'  // 排序方式
  showOnlyRecommended: boolean      // 只显示推荐模型
}
```

### 2. 内置模型组定义

```typescript
const BUILTIN_MODEL_GROUPS: ModelGroup[] = [
  {
    id: 'mainstream',
    name: '主流模型',
    description: 'GPT-4、Claude-3、Gemini 等主流商业模型',
    models: [
      'gpt-4', 'gpt-4-turbo', 'gpt-4o',
      'claude-3-opus', 'claude-3-sonnet', 'claude-3-haiku',
      'gemini-pro', 'gemini-1.5-pro'
    ],
    builtin: true
  },
  {
    id: 'gpt-series',
    name: 'GPT系列',
    description: 'OpenAI GPT 全系列模型',
    pattern: '^gpt-',
    builtin: true
  },
  {
    id: 'claude-series',
    name: 'Claude系列',
    description: 'Anthropic Claude 全系列模型',
    pattern: '^claude-',
    builtin: true
  },
  {
    id: 'opensource',
    name: '开源模型',
    description: 'Llama、Mistral、Qwen 等开源模型',
    pattern: '^(llama|mistral|mixtral|qwen|deepseek|yi)-',
    builtin: true
  }
]
```

### 3. 核心计算属性

```typescript
// 推荐模型：超过80%的上游账号都支持
const recommendedModels = computed(() => {
  const totalUpstreams = Object.keys(globalUpstreamModels.value).length
  if (totalUpstreams === 0) return []
  
  const threshold = totalUpstreams * 0.8
  return globalModelPool.value.filter(
    model => globalModelAccountCount(model) >= threshold
  )
})

// 筛选和排序后的模型列表
const filteredAndSortedModels = computed(() => {
  let models = globalModelPool.value
  
  // 1. 推荐过滤
  if (globalShowOnlyRecommended.value) {
    models = models.filter(m => recommendedModels.value.includes(m))
  }
  
  // 2. 文本筛选（支持正则）
  if (globalFilterText.value.trim()) {
    try {
      const regex = new RegExp(globalFilterText.value, 'i')
      models = models.filter(m => regex.test(m))
    } catch {
      // Fallback to simple string match
      const search = globalFilterText.value.toLowerCase()
      models = models.filter(m => m.toLowerCase().includes(search))
    }
  }
  
  // 3. 排序
  if (globalSortBy.value === 'support_count') {
    models = [...models].sort((a, b) => 
      globalModelAccountCount(b) - globalModelAccountCount(a)
    )
  } else {
    models = [...models].sort()
  }
  
  return models
})

// 获取模型组实际包含的模型
const getModelGroupModels = (group: ModelGroup): string[] => {
  if (group.models) {
    // 静态组：过滤出存在的模型
    return group.models.filter(m => globalModelPool.value.includes(m))
  }
  if (group.pattern) {
    // 动态组：使用正则匹配
    const regex = new RegExp(group.pattern, 'i')
    return globalModelPool.value.filter(m => regex.test(m))
  }
  return []
}
```

### 4. 批量操作方法

```typescript
// 全选当前筛选结果
const selectAllFilteredModels = () => {
  globalSelectedModels.value = [...filteredAndSortedModels.value]
}

// 反选当前筛选结果
const invertSelection = () => {
  const selected = new Set(globalSelectedModels.value)
  const filtered = filteredAndSortedModels.value
  
  globalSelectedModels.value = [
    // 保留不在筛选结果中的选中项
    ...globalSelectedModels.value.filter(m => !filtered.includes(m)),
    // 添加筛选结果中未选中的项
    ...filtered.filter(m => !selected.has(m))
  ]
}

// 清空所有选择
const clearSelection = () => {
  globalSelectedModels.value = []
}

// 应用模型组
const applyModelGroup = (group: ModelGroup) => {
  const groupModels = getModelGroupModels(group)
  const selected = new Set(globalSelectedModels.value)
  
  // 合并：保留原有选择 + 添加组模型
  groupModels.forEach(m => selected.add(m))
  globalSelectedModels.value = Array.from(selected)
}
```

## 🎨 UI 设计

### 对话框布局

```
┌─────────────────────────────────────────────────────────────┐
│  全局获取模型                                          [×]  │
├─────────────────────────────────────────────────────────────┤
│  已从 12 个上游账号获取到 156 个模型                        │
│                                                             │
│  ┌───────────────────────────────────────────────────────┐ │
│  │ 批量操作                                              │ │
│  │ [全选(156)] [反选] [清空]                             │ │
│  │                                                       │ │
│  │ 预设组合：                                            │ │
│  │ [主流模型(8)] [GPT系列(23)] [Claude系列(6)]          │ │
│  │ [开源模型(67)]                                        │ │
│  └───────────────────────────────────────────────────────┘ │
│                                                             │
│  ┌───────────────────────────────────────────────────────┐ │
│  │ 🔍 [搜索模型（支持正则）...          ]                │ │
│  │ 排序: [按支持数量▼]  ☑ 只显示推荐(覆盖率>80%)        │ │
│  └───────────────────────────────────────────────────────┘ │
│                                                             │
│  ┌───────────────────────────────────────────────────────┐ │
│  │ 已选 23/156 个模型                                    │ │
│  │                                                       │ │
│  │ ☑ gpt-4-turbo          (12个账号支持) ⭐           │ │
│  │ ☑ gpt-4                (12个账号支持) ⭐           │ │
│  │ ☑ claude-3-opus        (10个账号支持) ⭐           │ │
│  │ ☐ gpt-3.5-turbo        (11个账号支持) ⭐           │ │
│  │ ☐ llama-3-70b          (5个账号支持)              │ │
│  │ ☐ deepseek-coder       (3个账号支持)              │ │
│  │ ...                                                │ │
│  └───────────────────────────────────────────────────────┘ │
│                                                             │
│  ┌───────────────────────────────────────────────────────┐ │
│  │ 应用预览                                              │ │
│  │ 以下 12 个上游将新增模型                              │ │
│  │                                                       │ │
│  │ OpenAI-Main    → +3个 (gpt-4-turbo, gpt-4, ...)     │ │
│  │ Claude-US      → +2个 (claude-3-opus, ...)          │ │
│  │ ...                                                  │ │
│  └───────────────────────────────────────────────────────┘ │
│                                                             │
│                           [取消] [应用到支持的上游(23个)]   │
└─────────────────────────────────────────────────────────────┘
```

### 交互流程

1. **初始状态**
   - 展示所有模型（按支持数量降序）
   - 推荐模型带⭐标记
   - "已选 0/156"

2. **点击"主流模型"**
   - 自动选中 8 个主流模型
   - "已选 8/156"
   - 应用预览更新

3. **点击"GPT系列"**
   - 在现有选择基础上，增加所有 GPT 模型
   - "已选 23/156"（假设有15个新增的GPT模型）

4. **输入搜索"llama"**
   - 筛选结果：15个Llama模型
   - "已选 0/15"（显示筛选结果中的选中数）
   - 全选按钮变为"全选(15)"

5. **勾选"只显示推荐"**
   - 筛选结果：30个推荐模型
   - 其他筛选条件叠加生效

## 📁 文件修改清单

### 前端文件（只需修改1个文件）

**`frontend/src/views/admin/Upstreams.vue`**

修改位置：
- 第 1545 行附近：新增数据结构和响应式变量
- 第 1600 行附近：新增计算属性
- 第 1650 行附近：新增批量操作方法
- 第 535-610 行：修改全局获取模型对话框UI

### 后端文件（无需修改）

现有的 `/admin/upstreams/discover-models` 接口已满足需求。

## 🔧 实现细节

### 1. 模型筛选逻辑

```typescript
// 优先级：推荐过滤 > 文本筛选 > 排序
let models = globalModelPool.value

// Step 1: 推荐过滤（可选）
if (globalShowOnlyRecommended.value) {
  models = models.filter(m => recommendedModels.value.includes(m))
}

// Step 2: 文本筛选（可选）
if (globalFilterText.value) {
  const regex = new RegExp(globalFilterText.value, 'i')
  models = models.filter(m => regex.test(m))
}

// Step 3: 排序
if (globalSortBy.value === 'support_count') {
  models.sort((a, b) => globalModelAccountCount(b) - globalModelAccountCount(a))
} else {
  models.sort() // 按名称排序
}
```

### 2. 推荐算法

```typescript
// 覆盖率 = 支持该模型的上游数量 / 总上游数量
// 阈值 = 80%

const threshold = totalUpstreams * 0.8
const isRecommended = globalModelAccountCount(model) >= threshold

// 示例：
// - 总上游：10个
// - 阈值：8个
// - gpt-4 被 9 个上游支持 → 推荐✅
// - llama-3 被 3 个上游支持 → 不推荐❌
```

### 3. 正则筛选容错

```typescript
try {
  const regex = new RegExp(globalFilterText.value, 'i')
  models = models.filter(m => regex.test(m))
} catch (e) {
  // 如果用户输入的不是有效正则，fallback 到普通字符串匹配
  const search = globalFilterText.value.toLowerCase()
  models = models.filter(m => m.toLowerCase().includes(search))
}
```

### 4. 性能优化

- **使用 `computed()`**：所有派生状态都用计算属性，避免重复计算
- **避免深拷贝**：排序时使用 `[...models]` 创建浅拷贝
- **Set 去重**：使用 `Set` 进行模型去重，时间复杂度 O(1)

```typescript
// 高效的合并选择
const selected = new Set(globalSelectedModels.value)
groupModels.forEach(m => selected.add(m))  // O(n)
globalSelectedModels.value = Array.from(selected)
```

## ✅ 验收标准

### 功能验收

- [ ] 全选按钮能选中当前筛选结果的所有模型
- [ ] 反选按钮能正确反转选择状态（只反转筛选结果，不影响筛选外的选择）
- [ ] 清空按钮能清除所有选择
- [ ] 4 个预设模型组按钮能正确选中对应模型
- [ ] 预设按钮显示正确的模型数量（如 "GPT系列(23)"）
- [ ] 搜索框支持普通文本搜索
- [ ] 搜索框支持正则表达式（如 `^gpt-4`）
- [ ] 搜索框正则错误时 fallback 到普通搜索
- [ ] 排序功能正常（按名称 / 按支持数量）
- [ ] "只显示推荐"开关能正确筛选覆盖率>80%的模型
- [ ] 每个模型显示支持账号数量
- [ ] 推荐模型显示⭐图标
- [ ] 应用预览表格正确显示将要更新的上游

### 边界情况

- [ ] 0 个模型时：显示"未获取到任何模型"提示
- [ ] 筛选后 0 个结果时：显示"无匹配结果"提示
- [ ] 所有上游都失败时：显示友好的错误提示
- [ ] 选择0个模型时："应用"按钮禁用或提示
- [ ] 筛选后全选，然后取消筛选：之前筛选外的模型不受影响

### 用户体验

- [ ] 对话框宽度自适应（移动端 <720px）
- [ ] 筛选响应流畅（<100ms）
- [ ] 500+ 模型时无卡顿
- [ ] 操作按钮禁用状态正确
- [ ] Tooltip 显示清晰
- [ ] 键盘导航支持（Tab、Enter）

### 代码质量

- [ ] TypeScript 无类型错误
- [ ] Vue 组件无 ESLint 警告
- [ ] 所有新增方法有清晰的注释
- [ ] 计算属性使用 `computed()`
- [ ] 响应式变量使用 `ref()`
- [ ] 样式使用 scoped CSS

## 📊 性能指标

| 指标 | 目标 | 测试方法 |
|-----|------|---------|
| 筛选响应时间 | <100ms | 输入搜索文本后的渲染延迟 |
| 500个模型渲染 | <500ms | 虚拟滚动或分页 |
| 全选操作 | <50ms | 点击全选到UI更新 |
| 内存占用 | <50MB | Chrome DevTools Memory Profiler |

## 🚀 扩展功能（可选）

### 1. 用户自定义模型组

```typescript
interface UserModelGroup extends ModelGroup {
  userId: string
  createdAt: number
}

// 保存到 localStorage
const saveCustomGroup = (name: string, models: string[]) => {
  const group: UserModelGroup = {
    id: `custom-${Date.now()}`,
    name,
    description: '用户自定义',
    models,
    builtin: false,
    userId: currentUser.value.id,
    createdAt: Date.now()
  }
  
  const customGroups = JSON.parse(
    localStorage.getItem('custom_model_groups') || '[]'
  )
  customGroups.push(group)
  localStorage.setItem('custom_model_groups', JSON.stringify(customGroups))
}
```

### 2. 历史选择记忆

```typescript
// 记住上次的选择
onMounted(() => {
  const lastSelection = localStorage.getItem('last_global_model_selection')
  if (lastSelection) {
    globalSelectedModels.value = JSON.parse(lastSelection)
  }
})

// 保存选择
watch(globalSelectedModels, (newSelection) => {
  localStorage.setItem(
    'last_global_model_selection',
    JSON.stringify(newSelection)
  )
}, { deep: true })
```

### 3. 批量排除功能

```typescript
// "除了这些之外的所有模型"
const selectAllExcept = (excludedModels: string[]) => {
  const excluded = new Set(excludedModels)
  globalSelectedModels.value = globalModelPool.value.filter(
    m => !excluded.has(m)
  )
}

// UI: 右键菜单
<el-dropdown-item @click="selectAllExcept(getModelGroupModels(group))">
  选择此组之外的所有模型
</el-dropdown-item>
```

## 🎓 最佳实践

### 1. 响应式编程

```typescript
// ✅ Good: 使用计算属性
const filteredModels = computed(() => {
  return globalModelPool.value.filter(m => m.startsWith('gpt-'))
})

// ❌ Bad: 使用 watch 手动更新
watch(globalModelPool, (newPool) => {
  filteredModels.value = newPool.filter(m => m.startsWith('gpt-'))
})
```

### 2. 性能优化

```typescript
// ✅ Good: 使用 Set 去重
const selected = new Set(globalSelectedModels.value)
groupModels.forEach(m => selected.add(m))

// ❌ Bad: 使用数组 includes
groupModels.forEach(m => {
  if (!globalSelectedModels.value.includes(m)) {
    globalSelectedModels.value.push(m)
  }
})
```

### 3. 错误处理

```typescript
// ✅ Good: 优雅降级
try {
  const regex = new RegExp(filterText, 'i')
  return models.filter(m => regex.test(m))
} catch {
  // Fallback to simple search
  return models.filter(m => m.includes(filterText))
}

// ❌ Bad: 直接崩溃
const regex = new RegExp(filterText, 'i')
return models.filter(m => regex.test(m))
```

## 📚 参考资料

- [Element Plus Checkbox Group](https://element-plus.org/en-US/component/checkbox.html)
- [Vue 3 Computed](https://vuejs.org/guide/essentials/computed.html)
- [JavaScript RegExp](https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/RegExp)
- [TypeScript Generics](https://www.typescriptlang.org/docs/handbook/2/generics.html)

---

**文档版本：** 1.0  
**最后更新：** 2026-09-05  
**作者：** Claude Opus 5
