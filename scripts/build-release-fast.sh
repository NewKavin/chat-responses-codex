#!/usr/bin/env bash
# chat2Responses 快速 release 构建包装
# 痛点：环境变量 HTTPS_PROXY=http://127.0.0.1:7892(梯子) 让 cargo 访问国内镜像时绕到海外再回来，
#       首字节延迟从 0.28s 翻到 0.74s+，叠加几百个 crate 导致整体极慢。
# 方案：显式清掉所有代理变量，让 cargo 直连国内镜像（.cargo/config.toml 配的 tuna）。

set -euo pipefail
cd "$(dirname "$0")/.."

# 双保险：[http] proxy="" 在个别 cargo 版本不覆盖环境变量，这里强制 unset
unset HTTP_PROXY HTTPS_PROXY http_proxy https_proxy ALL_PROXY all_proxy

# 可选：启用 sccache 加速重复编译（如已装：cargo install sccache）
command -v sccache >/dev/null && export RUSTC_WRAPPER=sccache

# 时间统计 + 直接透传参数
time cargo build --release "$@"
