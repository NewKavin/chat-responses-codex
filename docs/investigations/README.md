# 调查文档索引

## 2026-09-04: 为什么 404 一直打上游，但 429/502 会停止？

**背景：** 内网聚合网关场景，需要"强制打上游抢占资源"，但 429/502 打几次就报路由耗尽。

**文档：**
1. **[调查总结](2026-09-04-why-404-always-hits-upstream.md)** - 完整的根因分析和解决方案
2. **[方案设计](2026-09-04-force-upstream-solution-design.md)** - 三种方案对比（推荐透传模式）
3. **[流程对比](2026-09-04-404-429-502-flow-comparison.md)** - 404/429/502 的完整执行路径

**核心发现：**
- 404 是"非临时失败"(`is_temporary() = false`)，不进入重试循环
- 429 被 B3 门拦截（默认）或被冷却阻断（B3 门打开后）
- 502 被冷却机制阻断（5s→10s→20s）

**解决方案：**
```bash
UPSTREAM_ROUTE_HEALTH_ENFORCEMENT_ENABLED=false  # 透传模式
UPSTREAM_RATE_LIMIT_INTERNAL_RETRY_ENABLED=true   # B3 门
```

**结果：** 429/502/503 都像 404 一样，每个请求都真实打上游。

---

**相关 Commit:**
- `54878119` - B3 门开关
- `1263cc42` - 透传模式
- `f22c083c` - Portal 权限管理（TDD 实现）
