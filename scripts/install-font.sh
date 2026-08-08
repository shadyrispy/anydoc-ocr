#!/usr/bin/env bash
# 安装内置 Noto Sans CJK 到系统/用户字体目录
# （ofd-core 渲染中文兜底；fontdb 直接扫描目录，无需 fc-cache 亦生效）
set -euo pipefail
DIR="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$DIR/fonts/NotoSansCJK-Regular.ttc"
[ -f "$SRC" ] || { echo "未找到字体: $SRC"; exit 1; }

if [ "$(id -u)" = "0" ]; then
  DEST="/usr/local/share/fonts"
else
  DEST="$HOME/.fonts"
fi
mkdir -p "$DEST"
cp -f "$SRC" "$DEST/"
echo "已安装: $DEST/NotoSansCJK-Regular.ttc"
command -v fc-cache >/dev/null && fc-cache -f "$DEST" >/dev/null 2>&1 || true
echo "完成（fontdb 直接扫描字体目录，无需重启）"
