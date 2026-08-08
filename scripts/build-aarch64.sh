#!/usr/bin/env bash
# aarch64 (linux-arm64) 交叉构建 + 离线打包
# 前置：第三方库需先放好（见 README §第三方库）
#   third_party/ort/aarch64/onnxruntime-linux-aarch64-1.20.1/{lib,include}
#   third_party/pdfium/aarch64/lib/libpdfium.so
#   fonts/NotoSansCJK-*.otf（OFD 渲染中文字体回退）
set -euo pipefail
cd "$(dirname "$0")/.."

TARGET=aarch64-unknown-linux-gnu
ORT_DIR="$PWD/third_party/ort/aarch64/onnxruntime-linux-aarch64-1.20.1"
PDFIUM_LIB="$PWD/third_party/pdfium/aarch64/lib"

export ORT_LIB_LOCATION="$ORT_DIR/lib"
export ORT_INCLUDE_LOCATION="$ORT_DIR/include"
export PDFIUM_LIB_DIR="$PDFIUM_LIB"
# ort 2.0 rc 对静态链接校验严格；用动态链接模式（运行时加载 .so）
export ORT_PREFER_DYNAMIC_LINK=1
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
# cc crate 可能编译少量 C（保险起见指向交叉编译器）
export CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc
export AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar
# 运行时相对自身 lib/ 目录加载原生库
export RUSTFLAGS="-C link-arg=-Wl,-rpath,\$ORIGIN/lib"

cargo build --release --target "$TARGET" "$@"

VERSION=$(grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)
OUT="dist/anydoc-ocr-linux-arm64"
rm -rf "$OUT" && mkdir -p "$OUT/lib"
cp "target/$TARGET/release/anydoc-ocr" "$OUT/"
cp "$ORT_LIB_LOCATION"/libonnxruntime.so* "$OUT/lib/"
cp "$PDFIUM_LIB"/libpdfium.so "$OUT/lib/" 2>/dev/null || true
[ -d fonts ] && cp -r fonts "$OUT/"
# 模型可选项：预置 oar-home 到 $OUT/oar-home 以实现离线
echo "Built -> $OUT"
