# 🚀 本地部署完成

## 服务信息

- **服务地址**: http://localhost:3002
- **启动时间**: 2026-05-23 16:09:54
- **运行状态**: ✅ 正常运行
- **数据存储**: data/state.json (文件模式)
- **日志文件**: logs/server.log

## 访问地址

### 管理后台
- **登录页面**: http://localhost:3002/admin/login
- **默认账号**: admin
- **默认密码**: admin (请在生产环境中修改)

### 门户页面
- **登录页面**: http://localhost:3002/portal/login
- **访问方式**: 使用下游配置的 API Key 登录

## 功能页面

### 管理后台功能
1. **仪表盘** - http://localhost:3002/admin
   - 系统概览
   - 统计信息

2. **上游管理** - http://localhost:3002/admin (上游标签页)
   - ✨ **实时状态显示** (每5秒自动刷新)
   - 并发数显示
   - 每分钟请求使用率 (进度条)
   - 5小时配额使用率 (进度条)
   - 高端模型保护状态
   - 智能路由配置

3. **下游管理** - http://localhost:3002/admin (下游标签页)
   - 下游配置管理
   - API Key 管理
   - 请求配额设置

4. **日志查看** - http://localhost:3002/admin (日志标签页)
   - 请求日志
   - 使用统计

### 门户功能
1. **概览页** - http://localhost:3002/portal
   - 请求配额使用情况 (进度条)
   - Token 使用统计
   - 模型使用摘要

2. **限额详情** - http://localhost:3002/portal (限额详情标签页)
   - 请求配额详情 (滑动窗口)
   - Token 配额详情
   - 模型白名单
   - IP 白名单

3. **使用历史** - http://localhost:3002/portal (使用历史标签页)
   - 每日统计图表
   - 最近请求日志

4. **模型目录** - http://localhost:3002/portal (模型目录标签页)
   - 可用模型列表
   - 模型使用统计

## 新功能亮点

### 1. 上游智能路由 🎯
- **高端模型保护**: 配置 `protect_premium_quota` 保护高端账号额度
- **智能调度**: 非高端模型请求自动避开保护账号
- **回退机制**: 无其他选项时自动回退

### 2. 实时状态监控 📊
- **进度条可视化**: 
  - 绿色 (< 60%): 正常
  - 橙色 (60-80%): 警告
  - 红色 (> 80%): 高负载
- **自动刷新**: 每5秒更新一次
- **详细指标**: 并发数、每分钟请求、5小时配额

### 3. 统一配额显示 📋
- **门户页面**: 统一使用"xxx小时，xxx次请求"格式
- **清晰展示**: 时间窗口、使用量、剩余量、百分比
- **一致性**: 与下游配置格式保持一致

## 当前数据

根据日志显示:
- ✅ 已加载 3 个上游配置
- ✅ 已加载 1 个下游配置
- ✅ 使用日志: 0 条

## 停止服务

```bash
# 查找进程
pgrep -f chat-responses-codex

# 停止服务
pkill -f chat-responses-codex
```

## 查看日志

```bash
# 实时查看服务日志
tail -f logs/server.log

# 查看运行时日志
tail -f logs/runtime.log
```

## 配置示例

### 高端账号保护配置
```json
{
  "id": "premium-glm",
  "name": "GLM 高端账号",
  "supported_models": ["gpt-4", "gpt-3.5-turbo", "glm-5.1"],
  "premium_models": ["glm-5.1"],
  "protect_premium_quota": true,
  "priority": 100,
  "requests_per_minute": 100,
  "request_quota_5h": 1000,
  "max_concurrency": 10
}
```

## 注意事项

1. **端口占用**: 如果 3002 端口被占用,可以修改 `BIND_ADDR` 环境变量
2. **数据持久化**: 所有配置保存在 `data/state.json` 文件中
3. **日志轮转**: 日志文件会自动轮转,保留最近的记录
4. **安全提醒**: 生产环境请修改默认密码并配置 HTTPS

## 技术栈

- **后端**: Rust + Axum
- **前端**: Vue 3 + Element Plus + TypeScript
- **数据存储**: JSON 文件 / PostgreSQL (可选)

---

🎉 **部署完成!** 现在可以通过浏览器访问 http://localhost:3002 查看效果了!
