# 修复 Portal 图表展示与 Token 展示

## TL;DR

> **Quick Summary**: 修复 UsageHistory.vue 中因 tab 切换导致图表尺寸异常的问题，并统一 Token 展示风格
>
> **Deliverables**:
> - UsageHistory.vue 图表正常渲染（支持 tab 可见性变化）
> - UsageHistory.vue Token 列使用图标展示（上/下/总 token）
> - Integration.vue Python SDK 示例移至 Claude Code tab 内
> - 移除冗余的 ModelCatalog.vue
>
> **Estimated Effort**: Short
> **Parallel Execution**: YES - 主要任务是单个文件修改

---

## Context

### Original Request
用户反馈以下问题：
1. 集成示例的 Python 示例需要提取到 Claude Code tab 后面
2. 使用历史的统计图表占位/显示异常
3. Token 需要展示和管理员日志记录一样（上/下/总 token + 图标）
4. 模型目录可删除

### Interview Summary
**已完成**：
- Integration.vue: Python SDK 示例已移到 Claude Code tab 内（`el-divider` + `python-sdk-section`）
- ModelCatalog.vue: 已删除，路由和 API 调用已清理
- UsageHistory Token 展示: 已改为 Top/Bottom/PieChart 图标

**待修复**：
- UsageHistory 图表：tab 激活前容器尺寸为 0，echarts.init() 捕获了错误尺寸
  - Dashboard.vue 正常因为它是全页面组件，不涉及 tab 隐藏
  - 需要添加 `ResizeObserver` 监听容器尺寸变化

---

## Work Objectives

### Core Objective
使 UsageHistory.vue 的图表在 tab 切换时能正确渲染

### Concrete Deliverables
- `frontend/src/views/portal/UsageHistory.vue` — 修改图表初始化逻辑

### Definition of Done
- [x] tab 切换到"使用历史"时图表正确显示（非空白/错位）
- [x] 切换时间范围（1天/7天/30天）时图表正常刷新
- [x] 前端构建通过（`npm run build`）

### Must Have
- ResizeObserver 监测图表容器尺寸变化
- 图表初始化在 DOM 渲染后进行
- 遵循 Dashboard.vue 的图表配置风格（grid/padding 等）

### Must NOT Have (Guardrails)
- 不改变 API 数据获取方式（保持现有 `portalApi.getUsageHistory()`）
- 不改变 Portal.vue 的结构

---

## Verification Strategy

> **ZERO HUMAN INTERVENTION** - ALL verification is agent-executed.

### Test Decision
- **Infrastructure exists**: YES (vitest, vue-tsc)
- **Automated tests**: NO
- **Agent-Executed QA**: YES — build verification + UI verification

### QA Policy
- Build verification: `npm run build` 通过
- UI verification: 使用 Playwright 打开页面并验证

---

## Execution Strategy

### Parallel Execution Waves

```
Wave 1 (Single Task):
├── Task 1: 修复 UsageHistory.vue 图表初始化问题

Wave FINAL:
├── Task F1: 构建前端验证
├── Task F2: Docker 部署验证
```

---

## TODOs

- [x] 1. 修复 UsageHistory.vue 图表初始化逻辑

  **What to do**:
  - 移除现有的 `initCharts` 方法，改用更健壮的初始化方式
  - 添加 `ResizeObserver` 监听两个图表容器（dailyChartRef 和 tokenChartRef）的尺寸变化，自动 resize
  - `onMounted` 流程改为：`await nextTick()` → `initCharts()` → `setupResizeObservers()` → `loadData()`
  - 每个图表单独拆分为独立更新函数（`updateDailyChart`, `updateTokenChart`）
  - 图表 grid 配置参考 Dashboard.vue：`grid: { left: 40, right: 20, top: 30, bottom: 30 }`
  - 图表高度改为 320px 与 Dashboard 保持一致
  - 每次 `updateCharts` 前先 `clear()` 再 `resize()` 再 `setOption()`，确保捕获正确容器尺寸
  - 在 `onUnmounted` 中清理 ResizeObserver

  **Must NOT do**:
  - 不要改变 Portal.vue 的结构
  - 不要改变 API 调用方式
  - 不要引入新的 npm 依赖

  **Recommended Agent Profile**:
  - **Category**: `visual-engineering`
    - Reason: 图表渲染 + Vue 组件修改
  - **Skills**: 无特定 skill 要求

  **Parallelization**:
  - **Can Run In Parallel**: NO (single task)
  - **Blocks**: F1, F2
  - **Blocked By**: None

  **References**:
  - `frontend/src/views/admin/Dashboard.vue:332-335,252-330` — 管理端图表正确实现（init + setOption + resize 模式）
  - `frontend/src/views/portal/UsageHistory.vue:24-31` — 图表容器（ref）
  - `frontend/src/views/portal/UsageHistory.vue:264-268` — onMounted 生命周期

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: 图表在 tab 切换后正确渲染
    Tool: Playwright
    Preconditions: 服务运行中，Portal 已登录
    Steps:
      1. 导航到 /portal#/portal
      2. 点击"概览"tab
      3. 点击"使用历史"tab
      4. 等待 2s 让图表渲染
      5. 截图保存
    Expected Result: 每日统计和 Token 使用趋势两个图表可见，非空白
    Evidence: .sisyphus/evidence/task-1-charts-visible.png

  Scenario: 时间范围切换后图表刷新
    Tool: Playwright
    Preconditions: 上述操作后，已在"使用历史"tab
    Steps:
      1. 点击"1 天"按钮
      2. 等待 2s
      3. 截图保存
    Expected Result: 图表数据更新
    Evidence: .sisyphus/evidence/task-1-charts-range-1d.png
  ```

  **Evidence to Capture**:
  - [x] task-1-charts-visible.png (tab 切换后图表可见)
  - [x] task-1-charts-range-1d.png (时间范围切换后)

  **Commit**: YES (groups with 1)
  - Message: `fix(portal): 修复使用历史图表在 tab 切换时显示异常`
  - Files: `frontend/src/views/portal/UsageHistory.vue`

---

## Final Verification Wave

- [x] F1. **构建验证** — `unspecified-high`
  运行 `cd frontend && npm run build`，确认无错误通过。

- [x] F2. **Docker 部署验证** — `unspecified-high`
  确认 Docker 构建并启动成功，Portal 页面可正常访问。

---

## Commit Strategy

- **1**: `fix(portal): 修复使用历史图表在 tab 切换时显示异常` - frontend/src/views/portal/UsageHistory.vue

---

## Success Criteria

### Verification Commands
```bash
cd /home/kavin/projects/chat2Responses/frontend && npm run build  # Expected: 构建成功，无错误
```

### Final Checklist
- [x] 图表在 tab 切换后正常显示
- [x] 图表时间范围切换正常
- [x] 构建通过
