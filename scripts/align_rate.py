#!/usr/bin/env python3
"""成对样本（扫描版 vs 文字版）类型序列 / 文本对齐率量化（#11）。

口径（BACKLOG #11 记录以本脚本为准）：
- 类型序列对齐率 = LCS(类型序列) / max(len)——LCS 的相等判据是 type 相同；
- 文本对齐率 = 对齐对中相似度达标的比例（difflib ratio，OCR 错字容忍），
  分母取两版该类型 item 数的较小者；同时输出 0.8（严格）与 0.6（宽松）两档；
- furniture（page_header/footer/number/footnote）是扫描版版面模型独有，
  对齐率分"全量"与"去 furniture（内容序列）"两口径。

用法: python3 scripts/align_rate.py <scan.json> <text.json>
"""
import json
import sys
from difflib import SequenceMatcher

import numpy as np

FURNITURE = {"page_header", "page_footer", "page_number", "page_footnote"}
STRICT, LOOSE = 0.8, 0.6


def items_of(path: str):
    """content-list-v2 → [(type, text)]；table 取 html，其余拼 paragraph/title content。"""
    doc = json.load(open(path, encoding="utf-8"))
    out = []
    for page in doc:
        for it in page:
            t = it.get("type", "?")
            c = it.get("content", {})
            if t == "table":
                text = c.get("html", "") if isinstance(c, dict) else ""
            else:
                key = "title_content" if t == "title" else "paragraph_content"
                parts = c.get(key) if isinstance(c, dict) else None
                text = (
                    "".join(x.get("content", "") for x in parts if isinstance(x, dict))
                    if isinstance(parts, list)
                    else ""
                )
            out.append((t, text))
    return out


def lcs_align(a, b):
    """LCS 回溯：相等判据 = type 相同。返回 [(i, j)] 对齐对（0-based）。"""
    n, m = len(a), len(b)
    dp = np.zeros((n + 1, m + 1), dtype=np.int32)
    for i in range(1, n + 1):
        ti = a[i - 1][0]
        row_eq = np.fromiter((b[j - 1][0] == ti for j in range(1, m + 1)), dtype=bool, count=m)
        dp[i, 1:][row_eq] = dp[i - 1, :-1][row_eq] + 1
        dp[i, 1:][~row_eq] = np.maximum(dp[i - 1, 1:][~row_eq], dp[i, :-1][~row_eq])
    pairs, i, j = [], n, m
    while i > 0 and j > 0:
        if a[i - 1][0] == b[j - 1][0] and dp[i, j] == dp[i - 1, j - 1] + 1:
            pairs.append((i - 1, j - 1))
            i, j = i - 1, j - 1
        elif dp[i - 1, j] >= dp[i, j - 1]:
            i -= 1
        else:
            j -= 1
    pairs.reverse()
    return pairs


def sim(x: str, y: str) -> float:
    if not x and not y:
        return 1.0
    sm = SequenceMatcher(None, x, y)
    if sm.real_quick_ratio() < LOOSE or sm.quick_ratio() < LOOSE:
        return sm.real_quick_ratio()
    return sm.ratio()


def report(a, b, label_a, label_b):
    pairs = lcs_align(a, b)
    print(f"═══ {label_a}({len(a)} items) vs {label_b}({len(b)} items) ═══")
    print(f"全量类型序列对齐率   LCS/max = {len(pairs)}/{max(len(a), len(b))} = {len(pairs)/max(len(a),len(b)):.1%}")

    af = [x for x in a if x[0] not in FURNITURE]
    bf = [x for x in b if x[0] not in FURNITURE]
    pairs_f = lcs_align(af, bf)
    print(f"内容序列对齐率(去 furniture) = {len(pairs_f)}/{max(len(af), len(bf))} = {len(pairs_f)/max(len(af),len(bf)):.1%}")

    from collections import Counter
    ca, cb = Counter(t for t, _ in a), Counter(t for t, _ in b)
    print("\n── 逐类型计数 ──")
    for t in sorted(set(ca) | set(cb)):
        print(f"  {t:14} {label_a}={ca.get(t,0):4}  {label_b}={cb.get(t,0):4}")

    print("\n── 文本对齐率（对齐对内相似度达标比例，分母=min(两版该类型数)）──")
    for t in ("paragraph", "title", "table"):
        sel = [(af[i][1], bf[j][1]) for i, j in pairs_f if af[i][0] == t]
        denom = min(ca.get(t, 0), cb.get(t, 0))
        if denom == 0:
            print(f"  {t:14} （某版为 0，跳过）")
            continue
        strict = sum(1 for x, y in sel if sim(x, y) >= STRICT)
        loose = sum(1 for x, y in sel if sim(x, y) >= LOOSE)
        print(f"  {t:14} 对齐对 {len(sel):4} | ≥{STRICT}: {strict}/{denom} = {strict/denom:.1%} | ≥{LOOSE}: {loose}/{denom} = {loose/denom:.1%}")

    print("\n── 未达标 paragraph 对抽样（对齐上了但文本漂移大，前 5）──")
    shown = 0
    for i, j in pairs_f:
        if af[i][0] != "paragraph":
            continue
        s = sim(af[i][1], bf[j][1])
        if s < LOOSE and shown < 5:
            shown += 1
            print(f"  [ratio={s:.2f}] 扫描版: {af[i][1][:60]!r}… | 文字版: {bf[j][1][:60]!r}…")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    a = items_of(sys.argv[1])
    b = items_of(sys.argv[2])
    report(a, b, "扫描版", "文字版")
