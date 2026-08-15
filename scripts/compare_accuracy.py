#!/usr/bin/env python3
"""对比文字版 ground truth 与扫描版 OCR 输出的字符级准确率。

策略：
1. 预处理：去空白/标点/英文/数字，只留中文字符（OCR 对标点空格处理差异大，干扰大）
2. 字符级编辑距离（Levenshtein）计算 CER = edit_dist / len(gt)
3. 分段对比：封面、目录、正文（按章节标题切分），定位错误集中区
"""
import re
import sys
from difflib import SequenceMatcher


def load(path):
    with open(path, encoding="utf-8") as f:
        return f.read()


def only_cjk(s):
    """只保留中日韩汉字，去掉一切标点空格数字英文——OCR 标点差异不计入错误。"""
    return "".join(re.findall(r"[\u4e00-\u9fff]", s))


def levenshtein(a, b):
    """字符级编辑距离。"""
    if not a:
        return len(b)
    if not b:
        return len(a)
    prev = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        cur = [i]
        for j, cb in enumerate(b, 1):
            cur.append(min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (ca != cb)))
        prev = cur
    return prev[-1]


def cer(gt, ocr):
    """字符错误率 = edit_dist / len(gt)。"""
    g = only_cjk(gt)
    o = only_cjk(ocr)
    if not g:
        return 0.0, 0, 0, 0
    d = levenshtein(g, o)
    return d / len(g), d, len(g), len(o)


def segment(md):
    """按章节标题（# 开头或数字编号行）切分段落。"""
    segs = []
    cur = []
    for line in md.splitlines():
        if re.match(r"^#{1,4}\s", line) or re.match(r"^\d+(\.\d+)*\s", line):
            if cur:
                segs.append("\n".join(cur))
            cur = [line]
        else:
            cur.append(line)
    if cur:
        segs.append("\n".join(cur))
    return segs


def find_diff_chars(gt, ocr, n=30):
    """提取前 n 个差异字符对，用于人工查看错误类型。"""
    g = only_cjk(gt)
    o = only_cjk(ocr)
    sm = SequenceMatcher(None, g, o, autojunk=False)
    diffs = []
    for tag, i1, i2, j1, j2 in sm.get_opcodes():
        if tag == "equal":
            continue
        g_part = g[i1:i2][:15]
        o_part = o[j1:j2][:15]
        diffs.append(f"[{tag}] GT='{g_part}' OCR='{o_part}'")
        if len(diffs) >= n:
            break
    return diffs


def main():
    gt_path = sys.argv[1]
    ocr_path = sys.argv[2]
    label = sys.argv[3] if len(sys.argv) > 3 else "OCR"

    gt = load(gt_path)
    ocr = load(ocr_path)

    # 全文 CER
    rate, dist, glen, olen = cer(gt, ocr)
    print(f"=== {label} ===")
    print(f"GT 中文字符数: {glen}")
    print(f"OCR 中文字符数: {olen}")
    print(f"编辑距离: {dist}")
    print(f"CER (字符错误率): {rate*100:.2f}%")
    print(f"准确率: {(1-rate)*100:.2f}%")
    print()

    # 分段对比
    gt_segs = segment(gt)
    ocr_segs = segment(ocr)
    print(f"GT 段数: {len(gt_segs)}, OCR 段数: {len(ocr_segs)}")
    print()

    print("--- 前 10 段对比 ---")
    for i in range(min(10, len(gt_segs), len(ocr_segs))):
        g_seg = gt_segs[i]
        o_seg = ocr_segs[i] if i < len(ocr_segs) else ""
        r, d, gl, ol = cer(g_seg, o_seg)
        title = g_seg.splitlines()[0][:40] if g_seg else "(空)"
        print(f"段{i} [{title}]: GT={gl}字 OCR={ol}字 距离={d} CER={r*100:.1f}%")

    print()
    print("--- 前 30 个差异字符 ---")
    for d in find_diff_chars(gt, ocr):
        print(f"  {d}")


if __name__ == "__main__":
    main()
