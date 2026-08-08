#!/usr/bin/env bash
# Usage: run_one.sh <input.pdf> <result_tag>
IN="$1"; TAG="$2"
WORK=$(mktemp -d)
OUT="$WORK/out.md"
ERR="$WORK/err.txt"
cd /workspace/anydoc-ocr
./run_ocr.sh "$IN" -o "$OUT" >/dev/null 2>"$ERR"
EC=$?
{
  echo "TAG=$TAG"
  echo "EXIT=$EC"
  echo "LINES=$(wc -l < "$OUT" 2>/dev/null || echo 0)"
  echo "ERR_TAIL:"; tail -8 "$ERR" 2>/dev/null
  echo "HEAD:"; head -15 "$OUT" 2>/dev/null
} > "/tmp/result_${TAG}.txt"
echo "done $TAG exit=$EC" >> /tmp/ocr_progress.log
