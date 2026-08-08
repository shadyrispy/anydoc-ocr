#!/usr/bin/env bash
# bench.sh <tag> <input> [extra args...]
# 记录：EXIT / 墙钟(s) / 行数 / 分阶段计时(ANYDOC_TIMINGS) / 持久化 markdown 供校准比对
TAG="$1"; IN="$2"; shift 2; EXTRA=("$@")
cd /workspace/anydoc-ocr
export ANYDOC_TIMINGS=1
mkdir -p /tmp/bench_out
OUT="/tmp/bench_out/${TAG}.md"
S=$(date +%s.%N)
./run_ocr.sh "$IN" -o "$OUT" "${EXTRA[@]}" >/dev/null 2>"/tmp/bench_${TAG}.err"
EC=$?
E=$(date +%s.%N)
WALL=$(awk "BEGIN{printf \"%.2f\", $E-$S}")
{
  echo "TAG=$TAG"
  echo "INPUT=$IN"
  echo "EXTRA=${EXTRA[*]}"
  echo "EXIT=$EC"
  echo "WALL_S=$WALL"
  echo "LINES=$(wc -l <"$OUT" 2>/dev/null || echo 0)"
  echo "TIMINGS:"
  grep '\[timing\]' "/tmp/bench_${TAG}.err"
} > "/tmp/bench_${TAG}.txt"
echo "done $TAG (${WALL}s)" >> /tmp/bench_progress.log
