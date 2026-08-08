#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

# x86_64 本地验证构建：预置 ORT + pdfium，走动态链接（随包分发 .so）
export ORT_LIB_LOCATION="$PWD/third_party/ort/x64/onnxruntime-linux-x64-1.20.1/lib"
export ORT_INCLUDE_LOCATION="$PWD/third_party/ort/x64/onnxruntime-linux-x64-1.20.1/include"
export PDFIUM_LIB_DIR="$PWD/third_party/pdfium/x64/lib"
# ort 2.0 rc 对静态链接校验严格；用动态链接模式（运行时加载 .so）
export ORT_PREFER_DYNAMIC_LINK=1

# 运行时库路径（开发期免设 LD_LIBRARY_PATH）
export LD_LIBRARY_PATH="$ORT_LIB_LOCATION:$PDFIUM_LIB_DIR:${LD_LIBRARY_PATH:-}"

cargo "$@"
