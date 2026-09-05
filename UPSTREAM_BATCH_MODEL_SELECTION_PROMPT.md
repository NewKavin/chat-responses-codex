# 开发任务：上游模型批量选择功能

## 📋 任务概述

为 chat2Responses 项目的"全局获取模型"功能添加批量选择能力，提升用户操作效率。

**核心目标：** 从手动勾选 23 次 → 一键选择

## 🎯 项目背景

**项目：** chat2Responses - AI 模型聚合网关  
**技术栈：** Rust (Axum) + Vue 3 (TypeScript) + PostgreSQL  
**当前版本：** 已有"全局获取模型"功能，但需要手动勾选每个模型

### 现有功能流程

1. 用户点击"全局获取模型"按钮
2. 系统并发调用所有上游账号的 `/v1/models` 接口
3. 展示去重后的模型列表（如 156 个模型）
4. 用户**手动勾选**每个想要的模型（❌ 效率低）
5. 点击"应用"后，系统将选中的模型合并到各上游的 `supported_models` 字段

### 痛点

- 用户想启用所有 GPT 模型，需要在 156 个模型中找到并手动勾选每一个 `gpt-*`
- 如果有 20 个 GPT 模型，需要点击 20 次
- 没有"全选"、"预设组"、"正则筛选"等便捷功能

## ✨ 需求功能

### 必需功能（MVP）

#### 1. 批量操作按钮
- **全选**：选中当前筛选结果的所有模型
- **反选**：反转当前筛选结果的选择状态（不影响筛选外的模型）
- **清空**：清除所有选择

#### 2. 预设模型组（至少 4 个）
| 组名 | 匹配规则 | 示例 |
|------|---------|------|
| 主流模型 | 固定列表 | gpt-4, claude-3-opus, gemini-pro |
| GPT系列 | 正则：`^gpt-` | gpt-4, gpt-4-turbo, gpt-3.5-turbo |
| Claude系列 | 正则：`^claude-` | claude-3-opus, claude-3-sonnet |
| 开源模型 | 正则：`^(llama\|mistral\|qwen\|deepseek)-` | llama-3-70b, mistral-large |

点击预设按钮后，自动选中该组的所有模型（累加到现有选择）。

#### 3. 筛选与排序
- **搜索框**：支持普通文本 + 正则表达式
  - 输入 `gpt` → 匹配包含 "gpt" 的模型
  - 输入 `^gpt-4` → 只匹配以 "gpt-4" 开头的模型
  - 正则错误时自动降级为普通搜索
  
- **排序方式**：
  - 按支持数量（降序）：显示最受欢迎的模型
  - 按名称（字母序）：方便查找特定模型

- **推荐过滤**：
  - 复选框"只显示推荐"
  - 推荐定义：超过 80% 的上游账号都支持的模型
  - 推荐模型显示⭐标记

#### 4. UI 增强
- 每个模型显示支持账号数量：`(12个账号支持)`
- 推荐模型显示⭐图标
- 显示当前选择状态：`已选 23/156`
- 应用预览表格正确显示将要更新的上游

### 可选功能（加分项）
- 用户自定义模型组（保存到 localStorage）
- 历史选择记忆（记住上次的选择）
- 右键菜单：选择此组之外的所有模型

## 📁 核心文件位置

### 唯一需要修改的文件

**`frontend/src/views/admin/Upstreams.vue`**

- **第 1543-1680 行**：全局获取模型的核心逻辑
  - `openGlobalFetch()` - 打开对话框并获取模型
  - `globalUpstreamModels` - 存储各上游的模型
  - `globalModelPool` - 去重后的所有模型
  - `globalSelectedModels` - 用户选中的模型
  - `applyGlobalModels()` - 应用选择到上游

- **第 535-610 行**：全局获取模型的 UI 对话框
  - `<el-dialog>` - 对话框容器
  - `<el-checkbox-group>` - 模型选择器
  - 应用预览表格

### 不需要修改的文件

- ❌ 后端 Rust 代码（`src/server/admin.rs`）
- ❌ 后端 API 接口（`/admin/upstreams/discover-models`）
- ❌ 数据库 Schema

## 🏗️ 技术实现指南

### Step 1: 新增数据结构

在 `<script setup>` 的第 1545 行附近新增：

```typescript
// 模型组定义
interface ModelGroup {
  id: string              // 唯一标识
  name: string            // 显示名称
  description: string     // 描述（用于 tooltip）
  pattern?: string        // 正则模式（动态组）
  models?: string[]       // 固定模型列表（静态组）
  builtin: boolean        // 是否为内置组
}

// 新增响应式变量
const globalFilterText = ref('')
const globalSortBy = ref<'name' | 'support_count'>('support_count')
const globalShowOnlyRecommended = ref(false)

// 内置模型组
const BUILTIN_MODEL_GROUPS: ModelGroup[] = [
  {
    id: 'mainstream',
    name: '主流模型',
    description: 'GPT-4、Claude-3、Gemini 等主流商业模型',
    models: ['gpt-4', 'gpt-4-turbo', 'gpt-4o', 'claude-3-opus', 'claude-3-sonnet', 'gemini-pro'],
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

### Step 2: 新增计算属性

```typescript
// 推荐模型（覆盖率 > 80%）
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

### Step 3: 新增批量操作方法

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

### Step 4: 修改 UI 模板

在第 535-610 行的 `<el-dialog>` 中，将原有的简单列表替换为：

```vue
<template v-else-if="globalModelPool.length > 0">
  <!-- 摘要 -->
  <div class="global-fetch-summary">
    已从 <strong>{{ Object.keys(globalUpstreamModels).length }}</strong> 个上游账号获取到
    <strong>{{ globalModelPool.length }}</strong> 个模型。
  </div>

  <!-- 批量操作工具栏 -->
  <div class="global-fetch-toolbar">
    <el-button-group>
      <el-button size="small" @click="selectAllFilteredModels">
        全选 ({{ filteredAndSortedModels.length }})
      </el-button>
      <el-button size="small" @click="invertSelection">反选</el-button>
      <el-button size="small" @click="clearSelection">清空</el-button>
    </el-button-group>

    <el-divider direction="vertical" />

    <span style="color: var(--el-text-color-secondary); font-size: 13px;">
      预设组合：
    </span>
    <el-button
      v-for="group in BUILTIN_MODEL_GROUPS"
      :key="group.id"
      size="small"
      plain
      @click="applyModelGroup(group)"
      :title="group.description"
    >
      {{ group.name }} ({{ getModelGroupModels(group).length }})
    </el-button>
  </div>

  <!-- 筛选与排序 -->
  <el-form :inline="true" style="margin-bottom: 12px;">
    <el-form-item>
      <el-input
        v-model="globalFilterText"
        placeholder="搜索模型（支持正则）"
        clearable
        style="width: 260px;"
      >
        <template #prefix>
          <el-icon><Search /></el-icon>
        </template>
      </el-input>
    </el-form-item>
    <el-form-item>
      <el-select v-model="globalSortBy" style="width: 140px;">
        <el-option label="按支持数排序" value="support_count" />
        <el-option label="按名称排序" value="name" />
      </el-select>
    </el-form-item>
    <el-form-item>
      <el-checkbox v-model="globalShowOnlyRecommended">
        只显示推荐 (覆盖率>80%)
      </el-checkbox>
    </el-form-item>
  </el-form>

  <!-- 模型选择列表 -->
  <div class="global-model-selection">
    <div class="selection-header">
      已选 <strong>{{ globalSelectedModels.length }}</strong> / 
      {{ filteredAndSortedModels.length }} 个模型
    </div>
    
    <el-checkbox-group v-model="globalSelectedModels" style="width: 100%;">
      <div
        v-for="model in filteredAndSortedModels"
        :key="model"
        class="model-list-item"
      >
        <el-checkbox :label="model" :value="model">
          <span class="model-name">{{ model }}</span>
          <span class="support-count">
            ({{ globalModelAccountCount(model) }} 个账号支持)
          </span>
          <el-icon
            v-if="recommendedModels.includes(model)"
            class="recommended-badge"
            :size="14"
          >
            <Star />
          </el-icon>
        </el-checkbox>
      </div>
    </el-checkbox-group>
  </div>

  <!-- 应用预览（保持原有代码） -->
  <div class="global-apply-preview" v-if="globalSelectedModels.length > 0">
    <!-- 原有的预览表格代码 -->
  </div>
</template>
```

### Step 5: 新增样式

在 `<style scoped>` 中添加：

```scss
.global-fetch-toolbar {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-bottom: 16px;
  flex-wrap: wrap;
  padding: 12px;
  background: var(--el-fill-color-lighter);
  border-radius: 4px;
}

.global-model-selection {
  max-height: 320px;
  overflow-y: auto;
  border: 1px solid var(--el-border-color);
  border-radius: 4px;
  padding: 8px;
  margin-bottom: 16px;

  .selection-header {
    padding: 8px 12px;
    background: var(--el-fill-color-light);
    border-radius: 4px;
    margin-bottom: 8px;
    font-size: 13px;
    color: var(--el-text-color-regular);
  }
}

.model-list-item {
  padding: 6px 8px;
  border-radius: 4px;
  transition: background 0.2s;

  &:hover {
    background: var(--el-fill-color-light);
  }

  .el-checkbox {
    width: 100%;
    display: flex;
    align-items: center;
  }

  .model-name {
    font-family: 'Monaco', 'Courier New', monospace;
    margin-right: 8px;
  }

  .support-count {
    color: var(--el-text-color-secondary);
    font-size: 12px;
    margin-left: auto;
  }

  .recommended-badge {
    color: var(--el-color-warning);
    margin-left: 4px;
  }
}
```

## 🎨 图标导入

如果项目中没有 `Search` 和 `Star` 图标，需要在 `<script setup>` 开头导入：

```typescript
import { Search, Star } from '@element-plus/icons-vue'
```

或者使用已有的图标库（项目中已安装 `@lucide/vue`）：

```typescript
import { Search, Star } from 'lucide-vue-next'
```

## ✅ 验收标准

### 功能测试

1. **批量操作**
   - [ ] 点击"全选"，选中所有当前筛选结果
   - [ ] 点击"反选"，反转当前筛选结果的选择状态
   - [ ] 点击"清空"，清除所有选择
   - [ ] 全选按钮显示正确的数量（如 "全选(156)"）

2. **预设模型组**
   - [ ] 点击"主流模型"，选中所有主流模型
   - [ ] 点击"GPT系列"，选中所有 gpt-* 模型
   - [ ] 点击"Claude系列"，选中所有 claude-* 模型
   - [ ] 点击"开源模型"，选中所有开源模型
   - [ ] 预设按钮显示正确的模型数量（如 "GPT系列(23)"）
   - [ ] 多次点击预设按钮，不会重复添加（使用 Set 去重）

3. **筛选功能**
   - [ ] 输入 "gpt" 能筛选出所有包含 gpt 的模型
   - [ ] 输入 "^gpt-4" 只筛选出 gpt-4 开头的模型
   - [ ] 输入无效正则（如 "[gpt"），自动降级为普通搜索
   - [ ] 清空搜索框，恢复显示所有模型

4. **排序功能**
   - [ ] 选择"按支持数排序"，模型按支持账号数降序排列
   - [ ] 选择"按名称排序"，模型按字母顺序排列

5. **推荐功能**
   - [ ] 勾选"只显示推荐"，只显示覆盖率>80%的模型
   - [ ] 推荐模型显示⭐图标
   - [ ] 推荐算法正确：支持账号数 ≥ 总账号数 * 0.8

6. **UI 显示**
   - [ ] 每个模型显示支持账号数量
   - [ ] 顶部显示"已选 X/Y"
   - [ ] 应用预览表格正确显示

### 边界测试

- [ ] 0 个模型时，显示友好提示
- [ ] 筛选后 0 个结果时，显示"无匹配结果"
- [ ] 所有上游都失败时，显示错误提示
- [ ] 选择 0 个模型时，"应用"按钮禁用
- [ ] 500+ 模型时，滚动流畅

### 性能测试

- [ ] 筛选响应时间 <100ms（输入后立即更新）
- [ ] 500 个模型渲染无卡顿
- [ ] 全选 500 个模型 <50ms

## ⚠️ 常见错误

### 1. 不要修改后端代码
所有功能都在前端实现，不需要改动 `src/server/admin.rs`。

### 2. 不要破坏现有功能
- 保持 `applyGlobalModels()` 方法不变
- 保持 `globalUpstreamModels` 数据结构不变
- 保持原有的获取流程不变

### 3. 不要引入新依赖
使用已有的：
- Vue 3 Composition API
- Element Plus 组件库
- TypeScript
- 图标库（Element Plus Icons 或 Lucide）

### 4. 注意类型安全
```typescript
// ✅ Good
const models = computed<string[]>(() => ...)

// ❌ Bad
const models = computed(() => ...) // 类型不明确
```

### 5. 注意性能
```typescript
// ✅ Good: 使用 Set 去重
const selected = new Set(globalSelectedModels.value)
groupModels.forEach(m => selected.add(m))

// ❌ Bad: 使用 includes
groupModels.forEach(m => {
  if (!globalSelectedModels.value.includes(m)) {
    globalSelectedModels.value.push(m)
  }
})
```

## 🚀 开发流程建议

### 阶段 1：核心功能（2小时）
1. 新增数据结构和响应式变量（30分钟）
2. 实现筛选和排序计算属性（30分钟）
3. 实现批量操作方法（30分钟）
4. 基础 UI 改造（30分钟）

### 阶段 2：预设模型组（1小时）
1. 定义内置模型组（15分钟）
2. 实现 `getModelGroupModels()`（15分钟）
3. 实现 `applyModelGroup()`（15分钟）
4. 添加预设按钮 UI（15分钟）

### 阶段 3：样式和优化（1小时）
1. 添加 CSS 样式（30分钟）
2. 添加图标和交互细节（15分钟）
3. 性能优化和边界处理（15分钟）

### 阶段 4：测试和修复（1小时）
1. 功能测试（30分钟）
2. 边界测试（15分钟）
3. Bug 修复（15分钟）

**总计：约 5 小时**

## 📚 参考资料

- [Element Plus 官方文档](https://element-plus.org/)
- [Vue 3 Composition API](https://vuejs.org/guide/extras/composition-api-faq.html)
- [JavaScript RegExp](https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/RegExp)
- [完整技术方案](./UPSTREAM_BATCH_MODEL_SELECTION_SPEC.md)

## 💡 提示

1. **增量开发**：先实现核心功能，再添加预设组和高级筛选
2. **频繁测试**：每完成一个功能就在浏览器测试
3. **使用 DevTools**：Vue DevTools 可以查看响应式数据
4. **参考现有代码**：项目中有很多类似的筛选和排序逻辑可以参考
5. **保持简洁**：优先考虑可读性，而不是过度优化

## 🤝 交付物

1. **代码文件**
   - 修改后的 `frontend/src/views/admin/Upstreams.vue`

2. **测试报告**
   - 验收标准清单（勾选完成的项）
   - 已知问题列表（如果有）

3. **使用说明**（可选）
   - 如何使用新功能的简短说明
   - 截图或 GIF 演示

---

**祝开发顺利！**

如有疑问，请参考：
- 完整技术方案：`UPSTREAM_BATCH_MODEL_SELECTION_SPEC.md`
- 现有代码：`frontend/src/views/admin/Upstreams.vue`
- Element Plus 文档：https://element-plus.org/
