#!/usr/bin/env bash
# 升级后批量基准：每样本跑 2 次（第 1 次含模型冷启，第 2 次稳态）。
set -uo pipefail
export ORT_LIB_LOCATION=/opt/ort/lib ORT_INCLUDE_LOCATION=/opt/ort/include ORT_PREFER_DYNAMIC_LINK=1
export PDFIUM_LIB_DIR=/opt/pdfium/lib OAR_HOME=/root/.oar
export LD_LIBRARY_PATH="/opt/ort/lib:/opt/pdfium/lib:${LD_LIBRARY_PATH:-}"
export ANYDOC_TIMINGS=1
BIN=/workspace/target/release/anydoc-ocr

mkdir -p /tmp/bench_out
PROG=/tmp/bench_progress.log
: > "$PROG"

run_one() {
  local tag="$1" in_path="$2" extra=("${@:3}")
  local out err s e ec wall lines
  out="/tmp/bench_out/${tag}.md"
  err="/tmp/bench_out/${tag}.err"
  s=$(date +%s.%N)
  "$BIN" "$in_path" -o "$out" "${extra[@]}" >/dev/null 2>"$err"
  ec=$?
  e=$(date +%s.%N)
  wall=$(awk "BEGIN{printf \"%.2f\", $e-$s}")
  lines=$(wc -l <"$out" 2>/dev/null || echo 0)
  {
    echo "TAG=$tag"
    echo "INPUT=$in_path EXTRA=${extra[*]}"
    echo "EXIT=$ec WALL_S=$wall LINES=$lines"
    echo "── stages (ms) ──"
    grep -E '^\[timing\] (stage|render|ocr|gfm|total) ' "$err" 2>/dev/null | head -20
    echo "── histogram ──"
    grep -E '^\[timing\] (render|ocr|total) ' "$err" 2>/dev/null | tail -3
  } > "/tmp/bench_out/${tag}.txt"
  echo "done $tag wall=${wall}s exit=$ec lines=$lines" >> "$PROG"
}

# 文字型（无 OCR）：1 次足够（无模型加载）
run_one text_pdf   /workspace/tests/samples/text.pdf
run_one text_ofd   /workspace/tests/samples/text.ofd
run_one text_font_ofd /workspace/tests/samples/text_font.ofd
run_one multipage_pdf /workspace/tests/samples/multipage.pdf
run_one real_table_pdf /workspace/tests/samples/real_table.pdf

# 图片型（OCR）：第 1 次冷启 + 第 2 次稳态；用扩展名区分 pdf/ofd
for smp in image.pdf image.ofd image_table.pdf image_table.ofd; do
  base="${smp%.*}"; ext="${smp##*.}"
  tag="${base}_${ext}_1"
  run_one "$tag" "/workspace/tests/samples/$smp"
  tag="${base}_${ext}_2"
  run_one "$tag" "/workspace/tests/samples/$smp"
done

echo "ALL_DONE" >> "$PROG"
cat "$PROG"
