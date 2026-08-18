#!/usr/bin/env bash
# mineru 分页批处理（规避沙箱 5.8Gi OOM）：逐批处理并合并 markdown。
# 用法: run_mineru_batch.sh <pdf> <outdir> <method> <batch_size>
set -uo pipefail
PDF="$1"; OUTDIR="$2"; METHOD="$3"; BATCH="${4:-5}"
MINERU=/tmp/eval/mineru_venv/bin/mineru
PYBIN=/tmp/eval/mineru_venv/bin/python3
PAGES=$($PYBIN - "$PDF" <<'PY'
import sys
from pypdf import PdfReader
print(len(PdfReader(sys.argv[1]).pages))
PY
)
echo "total_pages=$PAGES batch=$BATCH"
rm -rf "$OUTDIR"
mkdir -p "$OUTDIR"
MDLIST=""
for (( s=0; s<PAGES; s+=BATCH )); do
  e=$((s+BATCH-1)); [ $e -ge $PAGES ] && e=$((PAGES-1))
  TAG=$(printf "p%03d_%03d" $s $e)
  echo "=== batch pages $s-$e ==="
  OMP_NUM_THREADS=4 timeout 300 "$MINERU" -p "$PDF" -o "$OUTDIR/$TAG" -b pipeline -m "$METHOD" -l ch -s $s -e $e 2>&1 | grep -E "Completed batch|failed|Error|OCR-det|Layout Predict" | tail -3
  MD=$(find "$OUTDIR/$TAG" -iname '*.md' | head -1)
  if [ -n "$MD" ]; then
    echo "--- merged $MD ($(wc -l <"$MD") lines) ---"
    MDLIST="$MDLIST $MD"
  fi
done
# 合并（按页序）
echo "=== merging ==="
: > "$OUTDIR/merged_full.md"
for md in $MDLIST; do
  cat "$md" >> "$OUTDIR/merged_full.md"
  echo "" >> "$OUTDIR/merged_full.md"
done
echo "merged: $OUTDIR/merged_full.md ($(wc -l <"$OUTDIR/merged_full.md") lines)"