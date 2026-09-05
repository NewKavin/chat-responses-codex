# 上游批量选择模型功能 - TDD 开发计划

## 项目概述

**功能名称：** 上游批量选择模型（全局获取模型增强）

**目标：** 在现有"全局获取模型"功能基础上，增加批量选择能力，将手动勾选 N 个模型的操作简化为 1 次点击。

**影响文件：**
- `frontend/src/views/admin/Upstreams.vue`（主要改动）
- `frontend/src/utils/integration.ts`（可能需要辅助函数）

## 当前功能分析

### 现有代码位置
- **UI 对话框：** 第 535-610 行
- **逻辑实现：** 第 1543-1680 行
- **关键变量：**
  - `globalFetchDialogVisible`：控制对话框显示
  - `globalUpstreamModels`：各上游返回的模型列表
  - `globalSelectedModels`：用户选中的模型
  - `globalModelPool`：所有模型的去重列表

### 现有功能流程
1. 用户点击"全局获取模型"按钮
2. 系统并发调用所有上游的 `/v1/models` 接口
3. 展示去重后的模型列表（156 个模型）
4. 用户手动勾选每个模型 ❌ 痛点：需要多次点击
5. 点击"应用"，系统合并到各上游的 `supported_models`

## 需求功能清单

### Phase 1: 基础批量操作（MVP）
- [ ] 全选按钮：选中当前筛选结果的所有模型
- [ ] 反选按钮：反转当前选择状态
- [ ] 清空按钮：清除所有选择

### Phase 2: 预设模型组
- [ ] 主流模型组：固定列表（gpt-4, claude-3-opus 等）
- [ ] GPT 系列组：正则匹配 `^gpt-`
- [ ] Claude 系列组：正则匹配 `^claude-`
- [ ] 开源模型组：正则匹配 `^(llama|mistral|mixtral|qwen|deepseek)-`

### Phase 3: 筛选与排序
- [ ] 搜索框：支持普通文本和正则表达式
- [ ] 排序选择器：按名称/按支持数量
- [ ] 推荐开关：只显示覆盖率 >80% 的模型

### Phase 4: UI 增强
- [ ] 显示每个模型的支持账号数量
- [ ] 推荐模型显示 ⭐ 图标
- [ ] 显示已选数量 "已选 23/156"
- [ ] 应用预览表格

## TDD 测试策略

### 测试分类

#### 1. 单元测试（计算逻辑）
**文件：** `frontend/src/utils/integration.test.ts`（新建）

测试纯函数：
- `getModelGroupModels(group, allModels)` - 根据模型组定义返回匹配的模型
- `filterModelsByRegex(models, pattern)` - 正则筛选
- `calculateRecommendedModels(upstreamModels, threshold)` - 推荐算法

#### 2. 组件集成测试（用户交互）
**文件：** `frontend/src/views/admin/__tests__/Upstreams.batch.spec.ts`（新建）

测试场景：
- 点击"全选"后，所有模型被选中
- 点击"反选"后，选择状态翻转
- 点击"GPT 系列"后，所有 gpt- 开头的模型被选中
- 输入正则 `^claude-` 后，只显示 Claude 模型
- 勾选"只显示推荐"后，只显示覆盖率 >80% 的模型

#### 3. 端到端验收测试（手动）
**文件：** `UPSTREAM_BATCH_MODEL_SELECTION_E2E_CHECKLIST.md`（新建）

手动验证：
- 在真实浏览器中操作完整流程
- 验证应用后数据库确实更新
- 验证边界情况（0 个模型、1000+ 个模型）

## TDD 开发计划

### Sprint 1: 基础批量操作（2 小时）

#### Step 1.1: RED - 写测试（全选功能）
```typescript
// frontend/src/views/admin/__tests__/Upstreams.batch.spec.ts
describe('全局获取模型 - 批量选择', () => {
  it('点击全选按钮，应选中所有筛选结果中的模型', async () => {
    // 1. 模拟获取到 10 个模型
    // 2. 点击全选按钮
    // 3. 断言：globalSelectedModels.length === 10
  })
})
```

**预期：** 运行测试，失败（因为全选按钮还不存在）

#### Step 1.2: GREEN - 最小实现
```vue
<!-- frontend/src/views/admin/Upstreams.vue -->
<el-button @click="selectAllFilteredModels">全选</el-button>
```

```typescript
const selectAllFilteredModels = () => {
  globalSelectedModels.value = [...filteredAndSortedModels.value]
}
```

**预期：** 运行测试，通过 ✅

#### Step 1.3: REFACTOR - 优化
- 检查性能（如果模型 >1000 个）
- 检查代码重复
- 添加 JSDoc 注释

#### Step 1.4: RED - 写测试（反选功能）
```typescript
it('点击反选按钮，应反转当前筛选结果的选择状态', async () => {
  // 1. 模拟选中 5 个模型
  // 2. 点击反选按钮
  // 3. 断言：之前选中的变未选中，之前未选中的变选中
})
```

#### Step 1.5: GREEN - 实现反选
#### Step 1.6: REFACTOR
#### Step 1.7: RED - 写测试（清空功能）
#### Step 1.8: GREEN - 实现清空
#### Step 1.9: REFACTOR

**交付：**
- ✅ 全选/反选/清空按钮可用
- ✅ 所有测试通过
- ✅ 代码无 lint 错误

---

### Sprint 2: 预设模型组（2 小时）

#### Step 2.1: RED - 写纯函数测试
```typescript
// frontend/src/utils/integration.test.ts
describe('getModelGroupModels', () => {
  it('应正确返回 GPT 系列模型', () => {
    const allModels = ['gpt-4', 'gpt-3.5', 'claude-3-opus']
    const group = { pattern: '^gpt-' }
    const result = getModelGroupModels(group, allModels)
    expect(result).toEqual(['gpt-4', 'gpt-3.5'])
  })
})
```

**预期：** 失败（函数还不存在）

#### Step 2.2: GREEN - 实现纯函数
```typescript
// frontend/src/utils/integration.ts
export const getModelGroupModels = (group, allModels) => {
  if (group.pattern) {
    const regex = new RegExp(group.pattern, 'i')
    return allModels.filter(m => regex.test(m))
  }
  return group.models?.filter(m => allModels.includes(m)) || []
}
```

**预期：** 测试通过 ✅

#### Step 2.3: REFACTOR
#### Step 2.4: RED - 写组件集成测试
```typescript
it('点击"GPT 系列"按钮，应选中所有 GPT 模型', async () => {
  // 断言：globalSelectedModels 包含所有 gpt- 开头的模型
})
```

#### Step 2.5: GREEN - 实现 UI 按钮和点击逻辑
#### Step 2.6: REFACTOR
#### Step 2.7-2.12: 重复上述流程，实现其他 3 个模型组

**交付：**
- ✅ 4 个预设模型组按钮可用
- ✅ 点击后正确选中对应模型
- ✅ 所有测试通过

---

### Sprint 3: 筛选与排序（2 小时）

#### Step 3.1: RED - 写测试（正则搜索）
```typescript
it('输入正则 ^claude- 后，应只显示 Claude 模型', () => {
  // 设置 globalFilterText.value = '^claude-'
  // 断言：filteredAndSortedModels 只包含 claude- 开头的
})
```

#### Step 3.2: GREEN - 实现筛选逻辑
```typescript
const filteredAndSortedModels = computed(() => {
  let models = globalModelPool.value
  if (globalFilterText.value) {
    const regex = new RegExp(globalFilterText.value, 'i')
    models = models.filter(m => regex.test(m))
  }
  return models
})
```

#### Step 3.3: REFACTOR - 添加容错（无效正则降级为普通搜索）
#### Step 3.4-3.9: 实现排序和推荐开关

**交付：**
- ✅ 搜索框支持正则
- ✅ 排序功能正常
- ✅ 推荐开关正常

---

### Sprint 4: UI 增强（1 小时）

#### Step 4.1: 显示支持账号数量
```vue
<span class="support-count">({{ globalModelAccountCount(model) }} 个账号支持)</span>
```

#### Step 4.2: 推荐模型图标
```vue
<el-icon v-if="recommendedModels.includes(model)"><Star /></el-icon>
```

#### Step 4.3: 已选数量
```vue
<div>已选 {{ globalSelectedModels.length }} / {{ filteredAndSortedModels.length }}</div>
```

#### Step 4.4: 应用预览表格
（复用现有的 `globalApplyPreview` 计算属性）

**交付：**
- ✅ 所有 UI 元素显示正常
- ✅ 样式美观

---

## 验收标准

### 功能验收
- [ ] 全选按钮能选中当前筛选结果的所有模型
- [ ] 反选按钮能正确反转选择状态
- [ ] 4 个预设模型组按钮能正确选中对应模型
- [ ] 搜索框支持普通文本和正则表达式
- [ ] 排序功能正常（按名称/按支持数）
- [ ] "只显示推荐"开关能正确筛选覆盖率>80%的模型
- [ ] 每个模型显示支持账号数量
- [ ] 推荐模型显示⭐图标
- [ ] 应用预览表格正确显示将要更新的上游

### 代码质量
- [ ] 所有单元测试通过
- [ ] 所有组件测试通过
- [ ] TypeScript 无类型错误
- [ ] ESLint 无警告
- [ ] 所有新增方法有 JSDoc 注释

### 性能要求
- [ ] 筛选 1000+ 模型时响应 <100ms
- [ ] 选择 500+ 模型时无卡顿
- [ ] 对话框在移动端宽度合适

### 用户体验
- [ ] 空状态提示友好
- [ ] 操作按钮禁用状态正确
- [ ] 错误提示清晰（如：无效正则）

## 风险与应对

### 风险 1: 前端没有现成的测试框架
**应对：** 先建立测试环境（Vitest + Vue Test Utils）

### 风险 2: 大量模型时性能问题
**应对：** 使用虚拟滚动（`el-virtual-scroll`）

### 风险 3: 破坏现有功能
**应对：** 增量开发，不修改现有变量和方法，只新增

## 时间估算

| Sprint | 功能 | 预计时间 |
|--------|------|---------|
| Sprint 1 | 基础批量操作 | 2 小时 |
| Sprint 2 | 预设模型组 | 2 小时 |
| Sprint 3 | 筛选与排序 | 2 小时 |
| Sprint 4 | UI 增强 | 1 小时 |
| **总计** | | **7 小时** |

## 开始开发

执行以下命令开始 TDD 开发：

```bash
# 1. 调用 TDD skill
superpowers:test-driven-development

# 2. 创建测试文件
touch frontend/src/utils/integration.test.ts
touch frontend/src/views/admin/__tests__/Upstreams.batch.spec.ts

# 3. 启动测试监听模式
cd frontend && npm run test:watch
```

---

**文档版本：** 1.0  
**创建时间：** 2026-09-05  
**作者：** Claude Code  
**状态：** ✅ 计划完成，等待开发
