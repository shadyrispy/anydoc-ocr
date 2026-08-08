#!/usr/bin/env bash
# aarch64 离线打包：二进制 + ORT/PDFium 原生库 + 中文字体 + 离线模型 + 启动器
set -euo pipefail
cd "$(dirname "$0")/.."

TARGET=aarch64-unknown-linux-gnu
VERSION=$(grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)
BIN="target/$TARGET/release/anydoc-ocr"
[ -f "$BIN" ] || { echo "缺少 $BIN，先跑 scripts/build-aarch64.sh"; exit 1; }

OUT="dist/anydoc-ocr-linux-arm64"
rm -rf "$OUT" && mkdir -p "$OUT/lib" "$OUT/oar-home" "$OUT/fonts"

cp "$BIN" "$OUT/"
cp third_party/ort/aarch64/onnxruntime-linux-aarch64-1.20.1/lib/libonnxruntime.so* "$OUT/lib/"
cp third_party/pdfium/aarch64/lib/libpdfium.so "$OUT/lib/"
[ -d fonts ] && cp fonts/* "$OUT/fonts/" 2>/dev/null || true

# 离线模型：复用 auto-download 缓存布局（扁平 + .sha256 伴生文件，注意含隐藏文件）
if [ -d "$HOME/.oar" ] && ls -A "$HOME/.oar" | grep -q .; then
  cp -a "$HOME/.oar"/. "$OUT/oar-home/"
else
  echo "警告: $HOME/.oar 为空，离线包无模型（将走在线 auto-download）"
fi
echo "注意: oar-home 仅含本机已缓存档位（默认 tiny）；small/medium 档离线需先用对应档位跑过一次"

# 启动器：默认离线模型目录 + lib 路径（rpath 已覆盖，双保险）
cat > "$OUT/run.sh" <<'EOF'
#!/usr/bin/env bash
DIR="$(cd "$(dirname "$0")" && pwd)"
export OAR_HOME="${OAR_HOME:-$DIR/oar-home}"
export LD_LIBRARY_PATH="$DIR/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
exec "$DIR/anydoc-ocr" "$@"
EOF
chmod +x "$OUT/run.sh"

# 字体安装助手（ofd-core 渲染中文兜底）
if [ -f scripts/install-font.sh ]; then
  cp scripts/install-font.sh "$OUT/install-font.sh"
  chmod +x "$OUT/install-font.sh"
fi

# 校验产物架构
file "$OUT/anydoc-ocr" | grep -q aarch64 || { echo "错误：产物非 aarch64"; exit 1; }

tar czf "dist/anydoc-ocr-linux-arm64-${VERSION}.tar.gz" -C dist "anydoc-ocr-linux-arm64"
echo "打包完成: dist/anydoc-ocr-linux-arm64-${VERSION}.tar.gz"
du -sh "$OUT"
