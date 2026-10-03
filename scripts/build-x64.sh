#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

# x86_64 本地验证构建：预置 ORT + pdfium，走动态链接（随包分发 .so）
export ORT_LIB_LOCATION="$PWD/third_party/ort/x64/onnxruntime-linux-x64-1.28.2/lib"
export ORT_INCLUDE_LOCATION="$PWD/third_party/ort/x64/onnxruntime-linux-x64-1.28.2/include"
export PDFIUM_LIB_DIR="$PWD/third_party/pdfium/x64/lib"
# ort 2.0 rc 对静态链接校验严格；用动态链接模式（运行时加载 .so）
export ORT_PREFER_DYNAMIC_LINK=1

# 运行时库路径（开发期免设 LD_LIBRARY_PATH）
export LD_LIBRARY_PATH="$ORT_LIB_LOCATION:$PDFIUM_LIB_DIR:${LD_LIBRARY_PATH:-}"

# 链接器换GNU ld（容器 overlayfs 专用，见 BACKLOG「构建环境坑」）：
# rustc 默认的 rust-lld 用 mmap 写输出文件，本仓 lib test 二进制约 480MB，
# 在 overlayfs 上稳定触发 SIGBUS（`ld terminated with signal 7`）；GNU ld 走
# write 系统调用，同一产物可正常产出。**只影响 x86_64 本地验证构建**——
# 部署目标（aarch64）的链接参数在 .cargo/config.toml 里，不受此处影响。
#
# ⚠️ 代价：RUSTFLAGS 变更会让 cargo 认为全部产物失效并**全量重编**
# （本仓 970 crate / target 已18G，冷编~15-25 分钟）。改这行前先确认
# 现有 target 缓存是否还需要复用。
export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-C link-arg=-fuse-ld=bfd"

cargo "$@"
