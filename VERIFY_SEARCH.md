# 手动验证搜索功能

## 启动应用
```bash
# 终端 1：启动后端
cargo run --release

# 终端 2：启动前端
cd frontend && npm run dev
```

## 验证清单

### 1. 搜索框渲染 ✓
- [ ] 打开 http://localhost:5173/admin/settings
- [ ] 在状态栏下方看到搜索框
- [ ] 搜索框有放大镜图标
- [ ] placeholder 显示 "搜索配置项（名称、标签、描述）"

### 2. 搜索过滤 ✓
- [ ] 输入 "enforcement"
- [ ] 只显示包含该词的配置项
- [ ] 看到 "找到 N 项配置" 提示
- [ ] 清空搜索框，所有配置项恢复显示

### 3. 高亮显示 ✓
- [ ] 输入 "retry"
- [ ] 匹配的文字有黄色背景高亮
- [ ] 高亮出现在标题、key、描述中

### 4. 分组标签 ✓
- [ ] 输入 "timeout"（会匹配多个分组的配置）
- [ ] 每个字段右侧显示所属分组标签（如 "网络"、"路由"）
- [ ] 标签是淡蓝色

### 5. 无结果提示 ✓
- [ ] 输入 "xyz_nonexistent_12345"
- [ ] 显示 "未找到匹配「xyz_nonexistent_12345」的配置项"

### 6. 布局美化 ✓
- [ ] 字段行有 hover 效果（悬停变色）
- [ ] 间距合理、不拥挤
- [ ] 标签圆角、统一高度
- [ ] 字体大小清晰易读

### 7. 中文搜索 ✓
- [ ] 输入 "路由"
- [ ] 正确过滤中文标签和描述
- [ ] 中文高亮正常显示

### 8. 跨标签搜索 ✓
- [ ] 输入 "timeout"
- [ ] 搜索结果来自不同标签（网络、路由等）
- [ ] 分组标签帮助识别配置所属分类

## 预期搜索结果

| 搜索词 | 预期数量 | 包含配置 |
|--------|---------|---------|
| enforcement | ~3 | route_health_enforcement_enabled |
| retry | ~10 | *_retry_*, rate_limit_internal_retry |
| timeout | ~5 | *_timeout_seconds |
| 429 | ~2 | rate_limit_internal_retry_enabled |
| cooldown | ~5 | *_cooldown_* |
| upstream | ~50+ | upstream_* |

## 常见问题

**Q: 搜索框不显示？**
A: 确保已加载设置（editableSettings 不为 null）

**Q: 高亮不工作？**
A: 检查浏览器控制台是否有 v-html 警告

**Q: 分组标签不显示？**
A: 只在搜索模式下显示，清空搜索框时隐藏
