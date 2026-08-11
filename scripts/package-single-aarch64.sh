#!/usr/bin/env bash
# 单文件自解压安装器构建（方案 C）
#
# 产出 dist/anydoc-ocr-linux-arm64.run：
#   [自解压壳 bash 脚本][tar.gz 数据段]
# 目标机运行 .run：
#   - 首次：解压到 $ANYDOC_INSTALL_DIR（默认 ~/.anydoc-ocr）→ exec run.sh
#   - 再跑：直接 exec run.sh（免重复解压）
#   - --reinstall：强制重装后执行
#
# 资源（tiny 档内嵌）≈ 80-85M：bin + libonnxruntime.so + libpdfium.so +
# NotoSansCJK.ttc + tiny 模型 8 件（复用 package-aarch64.sh 的目录包）。
# small/medium 模型不内嵌，运行期用 ANYDOC_MODEL_DIR 外置（README）。
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=$(grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)
OUT="dist/anydoc-ocr-linux-arm64.run"
DIR_PKG="dist/anydoc-ocr-linux-arm64"
SELF_SIZE_SENTINEL="000000000000"   # 12 位定宽占位，替换后字节数不变

# 1. 先产出标准目录包（bin + lib + fonts + oar-home + run.sh）
scripts/package-aarch64.sh

# 2. 打 tar.gz（-C dist 保留顶层目录与 run.sh 可执行位）
TARBALL=$(mktemp)
trap 'rm -f "$TARBALL"' EXIT
tar czf "$TARBALL" -C dist anydoc-ocr-linux-arm64

# 3. 生成自解压壳（OFFSET 占位 12 位，构建时注入真实偏移）
SHELL_TMP=$(mktemp)
cat > "$SHELL_TMP" <<'SHELL_EOF'
#!/usr/bin/env bash
# anydoc-ocr 单文件安装器（自解压）。
# 用法: ./anydoc-ocr-linux-arm64.run [--reinstall] [anydoc-ocr 参数...]
set -euo pipefail
DIR="${ANYDOC_INSTALL_DIR:-$HOME/.anydoc-ocr}"
OFFSET=000000000000
if [ "${1:-}" = "--reinstall" ]; then
  rm -rf "$DIR"
  shift
fi
if [ ! -x "$DIR/anydoc-ocr" ]; then
  mkdir -p "$DIR"
  # 解压尾部 tar.gz 段（OFFSET 为数据段起始，1-based）；strip-components=1
  # 去掉包内顶层目录 anydoc-ocr-linux-arm64/，使 run.sh/lib/fonts 平铺于 $DIR
  tail -c +$OFFSET "$0" | tar xz --strip-components=1 -C "$DIR"
  echo "已安装 anydoc-ocr 到 $DIR（之后直接运行本文件即可；--reinstall 重装）"
fi
exec "$DIR/run.sh" "$@"
exit 0
SHELL_EOF

SHELL_SIZE=$(wc -c < "$SHELL_TMP")
OFFSET=$((SHELL_SIZE + 1))   # tar.gz 紧跟壳最后一行换行符之后（1-based）
sed "s/OFFSET=${SELF_SIZE_SENTINEL}/OFFSET=$(printf '%012d' "$OFFSET")/" "$SHELL_TMP" > "$OUT"
cat "$TARBALL" >> "$OUT"
chmod +x "$OUT"
rm -f "$SHELL_TMP"

echo "单文件安装器: $OUT ($(du -h "$OUT" | cut -f1))"
echo "目标机: ./anydoc-ocr-linux-arm64.run [--reinstall] [参数...]"
