#!/usr/bin/env bash
# DPI 扫描：文字型 PDF 强制 OCR，扫不同渲染 DPI，测 (时间, 字符集内容恢复率)。
# 单进程串行，threads 固定传入，避免并发内存压力导致幽灵值。
set -u
PDF="${1:?usage: dpi_sweep.sh <pdf> [dpi...]}"
shift || true
DPIS=("$@")
if [ ${#DPIS[@]} -eq 0 ]; then DPIS=(200 150 120 100 80); fi

export ORT_LIB_LOCATION=/workspace/anydoc-ocr/third_party/ort/x64/onnxruntime-linux-x64-1.20.1/lib
export ORT_INCLUDE_LOCATION=/workspace/anydoc-ocr/third_party/ort/x64/onnxruntime-linux-x64-1.20.1/include
export ORT_PREFER_DYNAMIC_LINK=1
export PDFIUM_LIB_DIR=/workspace/anydoc-ocr/third_party/pdfium/x64/lib
export OAR_HOME=/root/.oar
export LD_LIBRARY_PATH="${ORT_LIB_LOCATION}:${PDFIUM_LIB_DIR}:${LD_LIBRARY_PATH:-}"
BIN=/workspace/anydoc-ocr/target/release/anydoc-ocr
CAL=/workspace/anydoc-ocr/tests/calibrate.py
THREADS=1

GT=/tmp/dpi_gt.md
HYP=/tmp/dpi_hyp.md

if [ ! -s "$GT" ]; then
  "$BIN" "$PDF" > "$GT" 2>/dev/null
fi
echo "GT 字符数: $(wc -m < "$GT")"
echo "=== DPI 扫描 (threads=$THREADS, 单进程串行) ==="
printf "%-6s %-12s %-14s %-12s %-10s\n" DPI 耗时s 渲染ms OCR_s 字符集恢复

for D in "${DPIS[@]}"; do
  T0=$(date +%s.%N)
  ANYDOC_TIMINGS=1 "$BIN" "$PDF" --pdf-force-ocr --dpi "$D" --threads "$THREADS" > "$HYP" 2>/tmp/dpi_${D}_t.txt
  T1=$(date +%s.%N)
  WALL=$(awk "BEGIN{printf \"%.1f\", $T1-$T0}")
  RENDER=$(grep -oE '\[timing\] render: [0-9]+ms' /tmp/dpi_${D}_t.txt | grep -oE '[0-9]+' | head -1)
  OCR=$(grep -oE '\[timing\] ocr: [0-9.]+s' /tmp/dpi_${D}_t.txt | grep -oE '[0-9.]+' | head -1)
  CHR=$("$CAL" "$GT" "$HYP" "dpi$D" | grep -oE '字符集\] [0-9.]+' | grep -oE '[0-9.]+')
  printf "%-6s %-12s %-14s %-10s\n" "$D" "$WALL" "${RENDER:-?}ms" "${OCR:-?}s" "${CHR:-?}%"
done
