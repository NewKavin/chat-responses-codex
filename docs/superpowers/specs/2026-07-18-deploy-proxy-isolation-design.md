# Deploy 本地构建与运行镜像设计

## 背景与根因

`scripts/deploy.sh` 原来直接执行仓库根目录的多阶段 `docker build`。该 Dockerfile
会在 Docker/BuildKit 内拉取 Node 和 Rust 工具链并编译前端、后端。即使 deploy 进程
清除了 `HTTP_PROXY`、`HTTPS_PROXY`、`ALL_PROXY` 及小写变量，真实构建仍在解析
`node:22-bookworm-slim` 时命中 Docker daemon 配置的 `127.0.0.1:7892` 代理并超时。

仓库已有 `scripts/build-package-image.sh`，顺序正是宿主机安装前端依赖、构建前端、运行
本地 `cargo build --release`，然后把生成的二进制放入临时运行镜像上下文。Rust 服务使用
`rust-embed` 从 `frontend/dist` 嵌入前端资源，因此前端构建必须先于 Cargo 构建。

## 目标

- deploy 在宿主机完成 npm 和 Rust 编译，避免 Docker 内拉取 Node/Rust 工具链和重复编译。
- Docker 阶段只封装本地生成的二进制为运行镜像。
- 保持现有镜像名/tag 参数、配置复制、Compose 启动顺序和 `--skip-*` 语义。
- 继续在 deploy 入口清空六个标准代理变量，使本地构建和 Docker 子进程都不继承宿主代理。
- 运行镜像继续以非 root 的 `app` 用户运行，并保留健康检查、卷和环境变量。

## 非目标与边界

- 不修改 Docker daemon、BuildKit builder 或用户的 `~/.docker/config.json`。
- 不在 deploy 中复制一套 npm/Cargo 编译逻辑；复用现有 `build-package-image.sh`。
- 不改变现有 Compose 对 gateway、PostgreSQL 和 Redis 的启动及重建决策；本次验收使用
  `--skip-start`，不会触碰运行中的容器。
- 这不是完全离线 Docker 构建：Docker 仍需本地存在或能获取 `debian:bookworm-slim` 运行时基础镜像；Node/Rust 工具链不再由 Docker 获取。

## 方案

### Deploy 编排

保留入口的代理隔离：

```bash
unset HTTP_PROXY HTTPS_PROXY ALL_PROXY http_proxy https_proxy all_proxy
```

将 deploy 的 build 分支从直接 `docker build` 改为调用现有 helper：

```bash
"$SCRIPT_DIR/build-package-image.sh" \
  --image "$IMAGE_NAME" \
  --tag "$IMAGE_TAG" \
  --skip-export
```

`--skip-export` 防止 deploy 额外生成 tar 包；helper 仍负责本地依赖安装、前端构建、
Cargo release 构建、临时上下文复制和运行镜像构建。`--skip-build` 仍完全跳过该调用，
后续 Compose 分支保持原样。

### 运行镜像一致性

更新 helper 生成的临时 Dockerfile，使其与主 `Dockerfile` 的运行阶段一致：创建 `app`
组和 UID 10001 用户，创建并授权 `/data`、`/logs`、`/home/app`，设置同样的环境变量、
健康检查、卷、`USER app` 和 entrypoint。这样本地构建模式不会改变运行时权限模型。

### 本地构建数据流

```text
宿主环境
  ├─ npm ci/install -> frontend/node_modules
  ├─ npm run build   -> frontend/dist
  ├─ cargo build --release -> target/release/chat-responses-codex
  └─ 临时 Docker context 只包含二进制
       └─ docker build -> chat-responses-codex:<tag>
```

Docker 构建只读取运行时 Debian 基础镜像和临时二进制，不读取整个源码树，也不执行
Node/Rust 编译步骤。

## 备选方案

### 在 deploy 中复制本地编译逻辑

可以把 npm、Cargo 和临时镜像上下文代码搬进 `deploy.sh`，但会产生两套容易漂移的构建
流程。已有 helper 已覆盖目标顺序，因此不采用。

### 继续使用 Docker 多阶段构建并修 daemon 代理

这需要修改宿主 Docker 配置，影响其他项目，且仍承担工具链下载和 Docker 内编译耗时，
不符合本次部署目标。

## 错误处理

- 本地 npm、Cargo 或 helper Docker 命令失败时，沿用各脚本的 `set -euo pipefail` 立即退出。
- helper 找不到本地 release 二进制时继续给出已有错误提示，不创建不完整镜像。
- 构建失败后不自动回退到旧镜像，也不继续执行 Compose；已有 `--skip-start` 行为保持不变。
- 临时上下文由 helper 的 trap 清理；验收使用的临时 deploy 目录在验证结束后显式删除。

## 测试与验收

### 自动化测试

在 `tests/scripts.rs` 使用临时仓库副本运行真实 `deploy.sh` 委托链：

1. 复制 deploy、local-build helper、Compose 和 env 模板到临时目录。
2. 用假的 npm、cargo、docker 可执行文件记录调用；假的 Cargo 在临时仓库生成可执行二进制。
3. 注入六个 sentinel 代理变量，断言 npm、Cargo、Docker 子进程均观察到 unset。
4. 断言 npm 安装/构建、Cargo release 构建和 runtime `docker build` 顺序及 image/tag 参数。
5. 断言生成的运行 Dockerfile 包含 `USER app` 和健康检查，避免权限回退。

### 命令验收

- `bash -n scripts/deploy.sh scripts/build-package-image.sh`
- `cargo test --test scripts`
- `cargo test`
- `scripts/deploy.sh --skip-start --deploy-dir <temporary-dir> --image chat-responses-codex --tag latest`
- `docker image inspect chat-responses-codex:latest`

真实验收只启动本地编译和镜像封装，不启动 Compose；成功后保留 `chat-responses-codex:latest`，
删除临时 deploy 目录，不删除数据库、Redis 或现有运行容器。

## 回滚

回滚 deploy build 分支到直接调用 Docker 多阶段构建，并保留入口 `unset`（或按需要一并
回滚）即可。该变更不修改数据库 schema、持久化数据或运行中的容器状态。
