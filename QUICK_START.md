# 快速启动指南

## 🚀 本地测试（开发环境）

```bash
# 启动后端（会自动服务前端静态资源）
./target/release/chat-responses-codex

# 访问
# 前端：http://localhost:3001
# Admin：http://localhost:3001/admin/settings
```

---

## 🐳 Docker 部署（生产环境）

### 本地运行
```bash
docker run -d \
  --name chat-responses \
  -p 3001:3001 \
  -v $(pwd)/data:/data \
  -v $(pwd)/logs:/logs \
  chat-responses-codex:latest

# 查看日志
docker logs -f chat-responses

# 停止
docker stop chat-responses
```

---

## 📦 内网部署（你的场景）

### 1. 传输镜像到内网
```bash
# 在开发机器上
scp chat-responses-codex-latest.tar user@192.168.x.x:/tmp/
```

### 2. 在内网机器上加载并运行
```bash
# 加载镜像
docker load -i /tmp/chat-responses-codex-latest.tar

# 创建数据目录
mkdir -p /data/chat-responses /logs/chat-responses

# 运行容器（内网强占模式）
docker run -d \
  --name chat-responses \
  --restart unless-stopped \
  -p 3001:3001 \
  -v /data/chat-responses:/data \
  -v /logs/chat-responses:/logs \
  -e UPSTREAM_ROUTE_HEALTH_ENFORCEMENT_ENABLED=false \
  -e UPSTREAM_RATE_LIMIT_INTERNAL_RETRY_ENABLED=true \
  -e UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_ROUNDS=6 \
  -e UPSTREAM_ERROR_BODY_EXCERPT_ENABLED=true \
  chat-responses-codex:latest

# 验证运行状态
docker ps | grep chat-responses
docker logs chat-responses | tail -20

# 健康检查
curl http://localhost:3001/health
```

---

## ⚙️ 内网强占模式配置说明

这些环境变量让网关在内网环境下**持续打上游**，不会因为 429/502 而停止重试：

```bash
# 关闭路由健康执行（透传模式）
# 效果：502/503 不再被冷却拦截，每个请求都真实打上游
UPSTREAM_ROUTE_HEALTH_ENFORCEMENT_ENABLED=false

# 打开 B3 门（429 也进入网关内重试）
# 效果：429 不立即交给客户端，而是在网关内重试
UPSTREAM_RATE_LIMIT_INTERNAL_RETRY_ENABLED=true

# 增加重试轮次（给 429 更多真实打上游的机会）
UPSTREAM_ROUTE_EXHAUSTION_RETRY_MAX_ROUNDS=6

# 启用错误体摘录（内网诊断需要）
UPSTREAM_ERROR_BODY_EXCERPT_ENABLED=true
```

**关键效果**：
- 429/502 都会像 404 一样「一直打上游」
- 适合内网单聚合网关场景
- 上游资源抢占（别人也在抢，不能让网关自己停）

---

## ✅ 验证新功能（搜索和美化）

### 1. 打开设置页面
```
http://localhost:3001/admin/settings
```

### 2. 测试搜索
| 输入 | 预期结果 |
|------|---------|
| `enforcement` | 显示路由健康执行相关配置，匹配文字高亮 |
| `retry` | 显示所有重试配置（~10 项） |
| `429` | 显示 B3 门开关 |
| `透传` | 显示透传模式配置 |
| `xyz123` | 显示「未找到匹配的配置项」 |

### 3. 验证美化效果
- ✅ 搜索框在顶部，有放大镜图标
- ✅ 字段行有 hover 效果（悬停变色）
- ✅ 间距合理，不拥挤
- ✅ 搜索时显示分组标签（淡蓝色）
- ✅ 匹配文字黄色高亮

---

## 🔍 故障排查

### 问题 1：容器启动失败
```bash
# 查看日志
docker logs chat-responses

# 常见原因：
# - 端口 3001 被占用 → 改用 -p 3002:3001
# - 数据目录权限问题 → chmod 777 /data/chat-responses
```

### 问题 2：前端 404
```bash
# 确认前端已打包到镜像中
docker exec chat-responses ls -la /app/frontend/dist/

# 如果没有，说明镜像构建时前端未构建，重新运行：
./scripts/build-package-image.sh
```

### 问题 3：上游 429 还是不打
```bash
# 检查环境变量是否生效
docker exec chat-responses env | grep UPSTREAM

# 检查运行时设置是否被数据库覆盖
# 访问 Admin > 设置 > 查看实际值

# 强制重新加载配置（删除持久化数据）
docker stop chat-responses
rm -rf /data/chat-responses/*
docker start chat-responses
```

---

## 📊 监控和日志

### 查看实时日志
```bash
# 容器日志
docker logs -f chat-responses

# 文件日志（如果挂载了 /logs）
tail -f /logs/chat-responses/chat-responses-codex.log
```

### 关键日志指标
```bash
# 搜索路由耗尽事件
docker logs chat-responses | grep "routes_exhausted"

# 搜索上游 429
docker logs chat-responses | grep "upstream_status.*429"

# 搜索上游 502
docker logs chat-responses | grep "upstream_status.*502"

# 验证透传模式是否生效（应该看不到 "Cooling" 状态）
docker logs chat-responses | grep "Cooling"
```

---

## 🎯 下一步

1. **配置上游**：Admin > Upstreams > 添加你的上游 API
2. **配置下游**：Admin > Downstreams > 生成下游 API Key
3. **测试请求**：用下游 Key 调用网关，观察上游请求行为
4. **调整参数**：根据实际效果在 Admin > 设置 中微调

---

**部署完成，可以开始使用了！** 🎉

有问题随时告诉我。
