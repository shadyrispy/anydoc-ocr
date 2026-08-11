#!/usr/bin/env bash
# 单文件自解压安装器构建（方案 C，多架构，自动检测）
#
# 用法: scripts/package-single.sh [auto|aarch64|x86_64]
#   auto   (默认) → dist/anydoc-ocr-linux.run（内嵌双架构，目标机自动检测）
#   aarch64       → dist/anydoc-ocr-linux-arm64.run（单架构）
#   x86_64        → dist/anydoc-ocr-linux-x86_64.run（单架构）
#
# 产物 = [自解压壳 bash 脚本][tar.gz 数据段]，新机器一键部署：
#   1. 壳按 uname -m 检测架构，只解压对应架构目录到 $ANYDOC_INSTALL_DIR
#      （默认 ~/.anydoc-ocr）
#   2. 自动把包内 CJK 字体装入用户字体目录（fontdb load_system_fonts 生效）
#   3. 在 ~/.local/bin 安装 `anydoc` 命令（启动器：设 OAR_HOME/LD_LIBRARY_PATH），
#      并把 ~/.local/bin 加入 PATH（追加 shell rc，幂等）
#   之后直接敲 `anydoc` 即可；--reinstall 重装。
#
# 内嵌资源（每架构）：bin + libonnxruntime.so* + libpdfium.so + NotoSansCJK.ttc
#           + tiny 模型 8 件（OAR_HOME 布局）。small/medium 不内嵌，
#           运行期 ANYDOC_MODEL_DIR 外置（README）。
set -euo pipefail
cd "$(dirname "$0")/.."

MODE="${1:-auto}"
case "$MODE" in
  auto|dual)  ARCHES="aarch64 x86_64"; OUT="dist/anydoc-ocr-linux.run";;
  aarch64)    ARCHES="aarch64"; OUT="dist/anydoc-ocr-linux-arm64.run";;
  x86_64)     ARCHES="x86_64"; OUT="dist/anydoc-ocr-linux-x86_64.run";;
  *) echo "未知模式: $MODE（支持 auto / aarch64 / x86_64）"; exit 1 ;;
esac

VERSION=$(grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)
SELF_SIZE_SENTINEL="000000000000"   # 12 位定宽占位，替换后字节数不变

arch_conf() {
  case "$1" in
    aarch64)
      TARGET=aarch64-unknown-linux-gnu
      PKG_DIR=anydoc-ocr-linux-arm64
      ORT_SO="third_party/ort/aarch64/onnxruntime-linux-aarch64-1.20.1/lib/libonnxruntime.so"
      PDFIUM_SO="third_party/pdfium/aarch64/lib/libpdfium.so"
      BIN="target/$TARGET/release/anydoc-ocr"
      ;;
    x86_64)
      TARGET=x86_64-unknown-linux-gnu
      PKG_DIR=anydoc-ocr-linux-x86_64
      ORT_SO="third_party/ort/x64/onnxruntime-linux-x64-1.20.1/lib/libonnxruntime.so"
      PDFIUM_SO="third_party/pdfium/x64/lib/libpdfium.so"
      BIN="target/release/anydoc-ocr"   # host 构建（无 triple 前缀目录）
      ;;
  esac
}

# 组装单架构目录包
pack_arch() {
  local ARCH="$1" TARGET PKG_DIR ORT_SO PDFIUM_SO BIN
  arch_conf "$ARCH"
  local STAGE="dist/$PKG_DIR"

  [ -f "$ORT_SO" ] || { echo "缺少 $ORT_SO（先放置 $ARCH 预编译库）"; exit 1; }
  [ -f "$PDFIUM_SO" ] || { echo "缺少 $PDFIUM_SO"; exit 1; }
  [ -f fonts/NotoSansCJK-Regular.ttc ] || { echo "缺少 fonts/NotoSansCJK-Regular.ttc（先跑 scripts/install-font.sh）"; exit 1; }
  [ -f "$BIN" ] || { echo "缺少 $BIN（先构建 $ARCH release）"; exit 1; }

  rm -rf "$STAGE" && mkdir -p "$STAGE/lib" "$STAGE/oar-home" "$STAGE/fonts"
  cp "$BIN" "$STAGE/"
  # 复制 .so* 全部变体（含多版本链接链；只 cp 链接会断链）
  cp "$ORT_SO"* "$STAGE/lib/" 2>/dev/null || true
  cp "$PDFIUM_SO"* "$STAGE/lib/" 2>/dev/null || true
  cp fonts/* "$STAGE/fonts/" 2>/dev/null || true

  # 离线模型：只内嵌 tiny 档 8 件（复用 auto-download 布局：扁平 + .sha256 伴生）
  local TINY_MODELS="pp-doclayout-s.onnx picodet_layout_1x_table.onnx \
pp-ocrv6_tiny_det.onnx pp-ocrv6_tiny_rec.onnx ppocrv6_tiny_dict.txt \
slanet_plus.onnx pp-lcnet_x1_0_table_cls.onnx table_structure_dict_ch.txt"
  local PACKED=0 m
  for m in $TINY_MODELS; do
    if [ -f "$HOME/.oar/$m" ]; then
      cp "$HOME/.oar/$m" "$STAGE/oar-home/"
      [ -f "$HOME/.oar/$m.sha256" ] && cp "$HOME/.oar/$m.sha256" "$STAGE/oar-home/"
      PACKED=$((PACKED + 1))
    fi
  done
  [ "$PACKED" -gt 0 ] || echo "警告: $HOME/.oar 无 tiny 模型，该架构离线包无模型（将在线下载）"
}

# 启动器（run.sh + PATH anydoc 命令共用同一 env 逻辑，内容一致）
make_launcher() {
  local STAGE="$1"
  cat > "$STAGE/run.sh" <<'EOF'
#!/usr/bin/env bash
DIR="$(cd "$(dirname "$0")" && pwd)"
export OAR_HOME="${OAR_HOME:-$DIR/oar-home}"
export LD_LIBRARY_PATH="$DIR/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
exec "$DIR/anydoc-ocr" "$@"
EOF
  chmod +x "$STAGE/run.sh"
}

for A in $ARCHES; do
  pack_arch "$A"
  make_launcher "dist/anydoc-ocr-linux-$([ "$A" = aarch64 ] && echo arm64 || echo x86_64)"
done

# 打 tar.gz（-C dist，含选中架构的顶层目录；壳按架构选成员解压）
TARBALL=$(mktemp)
trap 'rm -f "$TARBALL"' EXIT
TAR_ARGS=()
for A in $ARCHES; do
  TAR_ARGS+=("anydoc-ocr-linux-$([ "$A" = aarch64 ] && echo arm64 || echo x86_64)")
done
tar czf "$TARBALL" -C dist "${TAR_ARGS[@]}"

# 自解压壳：架构检测 + 一键部署（解压/字体/PATH anydoc 命令）
SHELL_TMP=$(mktemp)
cat > "$SHELL_TMP" <<'SHELL_EOF'
#!/usr/bin/env bash
# anydoc-ocr 单文件安装器（自解压，自动检测架构，一键部署）。
# 用法: ./anydoc-ocr-linux.run [--reinstall] [anydoc-ocr 参数...]
set -euo pipefail
DIR="${ANYDOC_INSTALL_DIR:-$HOME/.anydoc-ocr}"
OFFSET=000000000000

# 架构检测
ARCH=$(uname -m)
case "$ARCH" in
  aarch64|arm64) SUB=anydoc-ocr-linux-arm64 ;;
  x86_64|amd64) SUB=anydoc-ocr-linux-x86_64 ;;
  *) echo "不支持的架构: $ARCH"; exit 1 ;;
esac

if [ "${1:-}" = "--reinstall" ]; then
  rm -rf "$DIR"
  shift
fi

if [ ! -x "$DIR/anydoc-ocr" ]; then
  mkdir -p "$DIR"
  # 解压尾部 tar.gz 段（OFFSET 1-based）；--strip-components=1 去掉顶层
  # 架构目录，只解压本架构成员，内容平铺于 $DIR
  tail -c +$OFFSET "$0" | tar xz --strip-components=1 -C "$DIR" "$SUB"

  # 一键字体：包内 CJK 字体 → 用户字体目录（ofd-core 的 fontdb 走
  # load_system_fonts 不扫包内，必须装进系统扫描目录；用户级免 root、幂等）
  if [ -f "$DIR/fonts/NotoSansCJK-Regular.ttc" ]; then
    FD="${XDG_DATA_HOME:-$HOME/.local/share}/fonts"
    mkdir -p "$FD"
    TARGET_FONT="$FD/anydoc-ocr-NotoSansCJK-Regular.ttc"
    [ -f "$TARGET_FONT" ] || cp "$DIR/fonts/NotoSansCJK-Regular.ttc" "$TARGET_FONT"
  fi

  # 一键命令：~/.local/bin/anydoc 启动器（设 OAR_HOME/LD_LIBRARY_PATH 后 exec）
  BIN_DIR="$HOME/.local/bin"
  mkdir -p "$BIN_DIR"
  cat > "$BIN_DIR/anydoc" <<CMDEOF
#!/usr/bin/env bash
DIR="\${ANYDOC_INSTALL_DIR:-\$HOME/.anydoc-ocr}"
export OAR_HOME="\${OAR_HOME:-\$DIR/oar-home}"
export LD_LIBRARY_PATH="\$DIR/lib\${LD_LIBRARY_PATH:+:\$LD_LIBRARY_PATH}"
exec "\$DIR/anydoc-ocr" "\$@"
CMDEOF
  chmod +x "$BIN_DIR/anydoc"

  # PATH 追加（幂等）：~/.local/bin 不在 PATH 则写 shell rc
  if ! echo ":$PATH:" | grep -q ":$BIN_DIR:"; then
    RC="$HOME/.bashrc"
    [ -f "$HOME/.zshrc" ] && RC="$HOME/.zshrc"
    if ! grep -q '\.local/bin' "$RC" 2>/dev/null; then
      echo 'export PATH="$HOME/.local/bin:$PATH"' >> "$RC"
      echo "[anydoc-ocr] 已将 ~/.local/bin 加入 PATH（新终端生效，或 source $RC）"
    fi
  fi

  echo "安装完成：直接运行 anydoc 即可（如: anydoc 公文.pdf -o out.md）"
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
echo "目标机一键部署: ./$(basename "$OUT") [--reinstall] [参数...] → 之后用 anydoc 命令"
