# 多 Key 管理 + 模型分组设计自检报告

**日期：** 2026-09-03  
**审查范围：** 多 Key 管理主设计 + 模型分组附加设计  
**状态：** ✅ 已修复所有严重问题

---

## 📋 发现的问题

### 🔴 严重问题（已修复）

#### 1. Admin API 缺少权限校验 ⚠️

**问题描述：**
- 原设计中 Admin API（创建/修改/删除模型分组）没有权限检查
- 任何登录用户都能修改全局模型分组配置
- 存在严重的安全风险

**修复方案：**
- `portal_users` 表添加 `is_admin BOOLEAN DEFAULT FALSE` 字段
- 所有 Admin API handler 添加 `verify_admin` 中间件
- 返回 403 Forbidden 给非管理员用户

**修改文件：**
- `docs/superpowers/specs/2026-09-03-multi-key-management-design.md`
  - 添加 5.2 章节：管理员权限控制
  - Migration 中添加 `is_admin` 列
  - 后端实现中添加 `verify_admin()` 函数

---

#### 2. 结构体字段不一致导致重复工作 ⚠️

**问题描述：**
- 主计划（Tasks 1-12）的 `PortalDownstreamBindingWithLabel` 没有 `model_group_id`
- 模型分组计划（Task 17）又要修改这个结构体添加该字段
- 会导致 Task 17 需要回头修改 Task 2-5 的所有代码

**修复方案：**
- 主计划从 Task 1 开始就预留 `model_group_id` 字段，默认值 `'basic'`
- Migration 一次性添加该列：`model_group_id TEXT DEFAULT 'basic' REFERENCES model_groups(id)`
- Task 2-5 的所有查询、插入、更新都包含该字段
- Task 17 简化为只添加 LEFT JOIN 返回 `model_group_name`

**修改文件：**
- `docs/superpowers/specs/2026-09-03-multi-key-management-design.md`
  - Migration 添加 `model_group_id` 列
  - 结构体定义包含 `model_group_id: String`
- `docs/superpowers/plans/2026-09-03-multi-key-management.md`
  - Task 1: Migration 包含 `model_group_id`
  - Task 2: 查询方法返回 `model_group_id`
  - Task 3: 写操作接受 `model_group_id` 参数
  - Task 5: API handler 处理 `model_group_id`
  - Task 7: 前端类型包含 `model_group_id` 和 `model_group_name`
- `docs/superpowers/plans/2026-09-03-model-groups-implementation.md`
  - Task 17 简化为只修改查询添加 LEFT JOIN

---

#### 3. N+1 查询问题导致性能下降 🐌

**问题描述：**
- `portal_list_keys` handler 对每个 key 单独调用 `get_model_group()`
- 10 个 key = 1 次 list 查询 + 10 次 group 查询 = 11 次数据库往返
- 在慢速网络下会严重影响响应时间

**修复方案：**
- `list_downstream_bindings_with_labels` 使用 `LEFT JOIN model_groups`
- 一次查询返回所有数据，包括 `model_group_name`
- 结构体添加 `model_group_name: String` 字段

**修改文件：**
- `docs/superpowers/plans/2026-09-03-multi-key-management.md`
  - Task 2: 查询改为 LEFT JOIN，返回 `model_group_name`
- `docs/superpowers/plans/2026-09-03-model-groups-implementation.md`
  - Task 17: 使用优化后的查询，无需额外调用

**性能提升：**
- 查询次数：11 次 → 1 次（节省 90.9%）
- 响应时间：~100ms → ~10ms（假设每次查询 10ms）

---

### 🟡 中等问题（已修复）

#### 4. 缺少使用统计实现

**问题描述：**
- 设计文档和前端类型定义了 `last_used_at` 和 `usage_last_7days` 字段
- 但没有后端实现逻辑
- 前端会一直显示空值

**修复方案：**
- `response_history` 表添加 `downstream_id` 索引
- `list_downstream_bindings_with_labels` 使用 LEFT JOIN 查询最近使用时间和 7 天使用次数
- 统计查询使用 `WHERE created_at > NOW() - INTERVAL '7 days'`

**修改文件：**
- `docs/superpowers/specs/2026-09-03-multi-key-management-design.md`
  - 添加 4.3 章节：使用统计实现
  - 详细的 SQL 查询示例

---

#### 5. Migration 依赖关系不明确

**问题描述：**
- 模型分组 Migration 依赖主 Migration
- 但没有明确说明执行顺序
- 如果先执行 Task 13，`portal_user_downstreams` 表不存在会报错

**修复方案：**
- 通过统一 `model_group_id` 字段自然解决
- 主 Migration 包含 `model_group_id` 列和外键
- 模型分组 Migration 只需创建 `model_groups` 表和插入初始数据
- 两个 Migration 可以合并为一个，或保持独立但顺序明确

**修改文件：**
- 主计划 Task 1 的 Migration 现在包含完整的 `model_group_id` 定义
- 模型分组计划 Task 13 的依赖关系更清晰

---

## ✅ 自检通过的方面

### 数据库设计
- ✅ 外键约束正确（`ON DELETE SET DEFAULT` 保护）
- ✅ 索引覆盖所有查询路径
- ✅ 默认值合理（`label` 允许 NULL，应用层提供默认值）
- ✅ 约束完整（CHECK, UNIQUE, NOT NULL）

### API 设计
- ✅ RESTful 风格一致
- ✅ 错误处理统一（error code + message）
- ✅ HTTP 状态码语义正确
- ✅ 请求/响应类型完整

### 前端设计
- ✅ TypeScript 类型安全
- ✅ 组件职责单一
- ✅ 状态管理清晰
- ✅ 错误提示用户友好

### 测试覆盖
- ✅ 单元测试覆盖核心逻辑
- ✅ 端到端测试覆盖关键流程
- ✅ 边界条件测试充分
- ✅ 并发场景有考虑

### 安全性
- ✅ 密钥生成使用密码学安全的随机数
- ✅ sk- key 拒绝登录（前后端双重校验）
- ✅ SQL 注入防护（参数化查询）
- ✅ Admin 权限校验（修复后）

### 向后兼容
- ✅ 零停机 Migration（label 允许 NULL）
- ✅ 旧 key 自动迁移到 'all' 分组
- ✅ 非 Portal key 跳过模型校验
- ✅ API 字段可选（model_group_id）

---

## 📊 修改统计

### 设计文档
- **主设计文档**：新增 ~150 行（Admin 权限、使用统计、model_group_id）
- **模型分组设计**：无修改（已经完整）

### 实现计划
- **主计划**：修改 ~50 处（添加 model_group_id 支持）
- **模型分组计划**：简化 Task 17（删除 ~200 行重复代码）

### 代码影响
- **Migration**：+2 列（is_admin, model_group_id）
- **Rust 代码**：预计新增 ~300 行，删除 ~50 行
- **TypeScript 代码**：预计新增 ~200 行
- **测试代码**：预计新增 ~400 行

---

## 🚀 实施建议

### 执行顺序
1. **阶段 1**：主计划 Tasks 1-12（多 Key 管理 + model_group_id 预留）
2. **阶段 2**：模型分组计划 Tasks 13-22（Admin API + 网关校验）

### 风险控制
- ✅ 每个 Task 独立可测
- ✅ Task 粒度合理（2-5 分钟/步骤）
- ✅ 频繁 commit（每个 Task 一次）
- ✅ 测试先行（TDD）

### 预计工作量
- **主计划**：12-15 小时
- **模型分组**：9-13 小时
- **总计**：21-28 小时（3-4 个工作日）

---

## 📝 结论

**所有严重问题已修复，设计方案可以开始实施。**

主要改进：
1. 🔒 安全性增强（Admin 权限校验）
2. 🏗️ 架构统一（model_group_id 从头预留）
3. ⚡ 性能优化（N+1 查询修复）
4. 📊 功能完整（使用统计实现）
5. 📋 依赖清晰（Migration 顺序明确）

**可以开始实施了！** 🎉
