#!/usr/bin/env python3
"""更公平的准确率对比：去掉重复页眉页脚，只比较正文连续中文段。

问题：全文 Levenshtein 把页眉页脚重复插入算作错误，虚高 CER。
改进：按行去重（页眉页脚在每页重复出现），只保留首次出现的行，
再拼接做字符级对比。
"""
import re
import sys
from difflib import SequenceMatcher


def load(path):
    with open(path, encoding="utf-8") as f:
        return f.read()


def only_cjk(s):
    return "".join(re.findall(r"[\u4e00-\u9fff]", s))


def levenshtein(a, b):
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


def dedup_lines(md):
    """去重重复行（页眉页脚每页重复），保留首次出现顺序。"""
    seen = set()
    out = []
    for line in md.splitlines():
        # 归一化：去空白标点，只看中文内容是否重复
        key = only_cjk(line)
        if not key:
            continue
        if key in seen:
            continue
        seen.add(key)
        out.append(line)
    return "\n".join(out)


def cer(gt, ocr):
    g = only_cjk(gt)
    o = only_cjk(ocr)
    if not g:
        return 0.0, 0, 0, 0
    d = levenshtein(g, o)
    return d / len(g), d, len(g), len(o)


def main():
    gt_path = sys.argv[1]
    ocr_path = sys.argv[2]
    label = sys.argv[3] if len(sys.argv) > 3 else "OCR"

    gt_raw = load(gt_path)
    ocr_raw = load(ocr_path)

    # 原始对比
    r1, d1, gl1, ol1 = cer(gt_raw, ocr_raw)
    # 去重后对比
    gt_dedup = dedup_lines(gt_raw)
    ocr_dedup = dedup_lines(ocr_raw)
    r2, d2, gl2, ol2 = cer(gt_dedup, ocr_dedup)

    print(f"=== {label} ===")
    print(f"[原始] GT={gl1}字 OCR={ol1}字 距离={d1} CER={r1*100:.2f}% 准确率={(1-r1)*100:.2f}%")
    print(f"[去重] GT={gl2}字 OCR={ol2}字 距离={d2} CER={r2*100:.2f}% 准确率={(1-r2)*100:.2f}%")
    print()

    # 用 difflib 找最长匹配块，统计正确字符占比（更鲁棒于插入删除）
    g = only_cjk(gt_dedup)
    o = only_cjk(ocr_dedup)
    sm = SequenceMatcher(None, g, o)
    matching = sm.get_matching_blocks()
    matched = sum(m.size for m in matching)
    ratio = sm.ratio()
    print(f"[去重+ratio] 匹配字符={matched}/{len(g)} ({matched/len(g)*100:.2f}%)")
    print(f"[去重+ratio] SequenceMatcher ratio={ratio*100:.2f}%")
    print()

    # 错误类型统计
    ops = {"replace": 0, "delete": 0, "insert": 0}
    for tag, i1, i2, j1, j2 in sm.get_opcodes():
        if tag in ops:
            ops[tag] += max(i2 - i1, j2 - j1)
    print(f"错误类型: 替换={ops['replace']} 删除(GT有OCR无)={ops['delete']} 插入(OCR有GT无)={ops['insert']}")


if __name__ == "__main__":
    main()
