#!/usr/bin/env bash
# 干净串行 DPI 扫描：单进程、无并发，量 WALL 时间 + 内容恢复率（字符集召回 vs 文字型 GT）
set -u
cd /workspace/anydoc-ocr
BIN=./target/release/anydoc-ocr
PDF=tests/real_samples/上海公报2025第1期.pdf
export LD_LIBRARY_PATH="/workspace/anydoc-ocr/third_party/ort/x64/onnxruntime-linux-x64-1.20.1/lib:/workspace/anydoc-ocr/third_party/pdfium/x64/lib:${LD_LIBRARY_PATH:-}"
export OAR_HOME=/root/.oar

# 文字型真值 GT
$BIN "$PDF" --output /tmp/dpi_gt.md 2>/dev/null
echo "GT chars: $(wc -m < /tmp/dpi_gt.md)"

recall() {
  python3 - "$1" "$2" <<'PY'
import sys,re
def norm(s): return set(re.findall(r'[\u4e00-\u9fff0-9A-Za-z]', s))
gt=norm(open(sys.argv[1]).read()); hy=norm(open(sys.argv[2]).read())
print(f"{len(gt&hy)/len(gt)*100:.2f}")
PY
}

for dpi in 200 150 120 100 80; do
  start=$(date +%s.%N)
  ANYDOC_TIMINGS=1 $BIN "$PDF" --pdf-force-ocr --dpi $dpi --threads 4 \
    --output /tmp/dpi_${dpi}.md 2>/tmp/dpi_${dpi}.timing
  end=$(date +%s.%N)
  wall=$(awk -v a="$start" -v b="$end" 'BEGIN{printf "%.1f", b-a}')
  render=$(grep -oP 'render\s+\+\s+\K[0-9.]+' /tmp/dpi_${dpi}.timing)
  ocr=$(grep -oP 'ocr\s+\+\s+\K[0-9.]+' /tmp/dpi_${dpi}.timing)
  rc=$(recall /tmp/dpi_gt.md /tmp/dpi_${dpi}.md)
  printf "DPI=%-3s WALL=%-7s render=%6sms ocr=%8ss recall=%s%% out_chars=%s\n" \
    "$dpi" "${wall}s" "${render:-NA}" "${ocr:-NA}" "$rc" "$(wc -m < /tmp/dpi_${dpi}.md)"
done
