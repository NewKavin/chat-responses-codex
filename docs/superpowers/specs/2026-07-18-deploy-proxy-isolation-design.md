# Deploy 代理隔离设计

## 背景

`scripts/deploy.sh` 直接调用现有的 `docker build`。当启动 deploy 的宿主环境设置了
`HTTP_PROXY`、`HTTPS_PROXY`、`ALL_PROXY` 或对应的小写变量时，这些变量会被 Docker
调用继承。无网络的假 Docker 复现确认，六个变量目前都会原样进入 build 调用。

仓库中的 `scripts/build-release-fast.sh` 已通过清空同一组变量避免 Cargo 误用宿主代理，
但 deploy 路径尚未采用这个约束。

## 目标

- deploy 脚本运行期间禁用常见的大小写 HTTP、HTTPS 和 ALL 代理变量。
- 保持原有 Docker 构建命令、参数、镜像名称、配置复制和 Compose 启动顺序不变。
- 不修改调用 deploy 的父 shell 环境。
- 通过自动化测试和一次真实 deploy 构建证明行为生效。

## 非目标

- 不增加命令行开关。
- 不改变 Dockerfile、Cargo、npm 或 Compose 的编译和部署逻辑。
- 不修改 Docker daemon 或用户级 `~/.docker/config.json`。
- 不为代理失败增加重试或替代镜像源。

## 方案

在 `scripts/deploy.sh` 的 `set -euo pipefail` 之后立即执行：

```bash
unset HTTP_PROXY HTTPS_PROXY ALL_PROXY http_proxy https_proxy all_proxy
```

脚本是独立进程，因此 `unset` 只影响本次 deploy 及其子进程，不会修改父 shell。
代理在参数解析、Docker 检测和构建之前即被清空，后面的现有命令保持原样：

```bash
docker build -t "${IMAGE_NAME}:${IMAGE_TAG}" "$REPO_ROOT"
```

`NO_PROXY` 和 `no_proxy` 不需要清空：当代理变量不存在时它们不会启用代理，也不会改变
网络路径。变量集合与 `scripts/build-release-fast.sh` 保持一致。

## 备选方案

### 仅包装 build 命令

使用 `env -u ... docker build` 可以只隔离构建步骤，但会改变构建命令形式，并让 Compose
继续继承代理。它不符合“deploy 脚本禁用代理”和“原有编译逻辑不变”的约束。

### 传递空 build args

向 Docker 增加六个空代理 build args 会改变构建参数，而且无法禁用 Docker CLI 自身继承
的代理环境。该方案范围更大，故不采用。

## 错误处理

`unset` 对未定义变量返回成功，并且与 `set -u` 兼容，不需要条件分支。后续 Docker 构建
或 Compose 失败时继续沿用现有的 `set -e` 退出行为，不吞掉错误，也不添加隐式回退。

## 测试

在 `tests/scripts.rs` 增加 deploy 行为测试：

1. 使用临时部署目录和假 Docker 可执行文件运行 deploy，并设置六个 sentinel 代理变量。
2. 假 Docker 在 `docker build` 调用处记录六个变量是否存在以及收到的参数。
3. 断言六个代理变量全部不存在。
4. 断言构建参数仍是 `build -t <image>:<tag> <repo-root>`，证明原编译调用未改变。
5. 使用 `--skip-start` 避免测试启动服务。

实现后执行：

- `bash -n scripts/deploy.sh`
- 对应的 `cargo test --test scripts` 测试
- 完整 Rust 测试
- `scripts/deploy.sh --skip-start` 的真实 Docker 构建

## 验收标准

- 带六个代理变量启动 deploy 时，`docker build` 环境中这些变量均未定义。
- deploy 中现有的 Docker build、配置复制和 Compose 逻辑没有结构性变化。
- 自动化测试通过。
- 真实 deploy 构建成功并生成目标镜像。
- `main` 中不残留临时部署目录、测试捕获文件或临时镜像。

## 回滚

回滚时删除入口处的单行 `unset` 及对应测试即可。该变更不涉及数据库、配置格式、镜像
内容迁移或运行时状态，因此无需数据回滚。
