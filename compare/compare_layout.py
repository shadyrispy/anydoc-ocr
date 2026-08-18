#!/usr/bin/env python3
"""版面格式对比 + OCR 准确率校验。

groundtruth = 文字版 PDF 文字层（anydoc_text.md，直接读 PDF 内嵌文本）。
OCR 候选 = anydoc_scan.md（anydoc OCR）、mineru_scan/merged_full.md（mineru pipeline OCR）。
"""
import re, sys, unicodedata, collections
from difflib import SequenceMatcher

def heading_levels(path):
    levels = collections.Counter()
    for ln in open(path, encoding='utf-8'):
        m = re.match(r'^(#{1,6})\s', ln)
        if m:
            levels[len(m.group(1))] += 1
    return levels

def table_count(path):
    n_html = 0
    n_pipe_rows = 0
    in_code = False
    for ln in open(path, encoding='utf-8'):
        s = ln.strip()
        if s.startswith('<table'):
            n_html += 1
        # pipe 表分隔行：形如 |---|---|
        if re.match(r'^\|[\s:|-]+\|$', s) and set(s) <= set('|:- \t'):
            n_pipe_rows += 1
    return dict(html=n_html, pipe_sep_rows=n_pipe_rows)

def clean_text(path):
    """去 markdown 标记、页眉页脚、页码、空白，保留正文。用于 OCR 准确率。"""
    txt = []
    for ln in open(path, encoding='utf-8'):
        s = ln.rstrip('\n')
        if s.startswith('!['):
            continue
        s = re.sub(r'^#{1,6}\s*', '', s)
        s = re.sub(r'^\|', '', s)
        s = re.sub(r'^\||\|$', '', s)
        s = re.sub(r'^\s*[-*]\s+', '', s)
        s = re.sub(r'^>\s*', '', s)
        # 去掉表格分隔行
        if set(s) <= set('|:- \t') and s.count('|') >= 2:
            continue
        txt.append(s.strip())
    body = '\n'.join(txt)
    # 页眉/页脚/页码归一化剔除：GJB 9001C—2017、纯数字行
    lines = [l for l in body.split('\n') if l.strip() and not re.fullmatch(r'\d{1,3}', l.strip())
             and 'GJB 9001C' not in l and '........' not in l]
    return '\n'.join(lines)

def cjk(s):
    return re.findall(r'[\u4e00-\u9fff]', s)

def cer(gt, cand):
    """字符级编辑距离（Levenshtein）→ CER。"""
    n = len(gt)
    if n == 0:
        return 1.0 if cand else 0.0
    # 仅用 CJK 序列，避免标点/数字/英文干扰
    g = cjk(gt)
    c = cjk(cand)
    prev = list(range(len(c)+1))
    for i, gc in enumerate(g, 1):
        cur = [i]*(len(c)+1)
        for j, cc in enumerate(c, 1):
            cost = 0 if gc == cc else 1
            cur[j] = min(prev[j]+1, cur[j-1]+1, prev[j-1]+cost)
        prev = cur
    dist = prev[-1]
    return dist / len(g)

def aligned_ratio(gt, cand):
    g = ''.join(cjk(gt)); c = ''.join(cjk(cand))
    return SequenceMatcher(None, g, c).ratio()

files = {
    'GT(文字层)': 'anydoc_text.md',
    'anydoc_scan': 'anydoc_scan.md',
    'mineru_scan': 'mineru_scan/merged_full.md',
    'mineru_text': 'mineru_text/merged_full.md',
}
cache = {}
for name, f in files.items():
    cache[name] = (heading_levels(f), table_count(f), clean_text(f))

print('=== 版面格式对比 ===')
print(f"{'源':<14}{'H1':>4}{'H2':>4}{'H3':>4}{'H4':>4}{'H5':>4}{'H6':>4}  <table>  管道表分隔行")
for name, f in files.items():
    h, t, _ = cache[name]
    row = f"{name:<14}"
    for lv in range(1,7):
        row += f"{h.get(lv,0):>4}"
    row += f"   {t['html']:>6}   {t['pipe_sep_rows']:>6}"
    print(row)

gt = cache['GT(文字层)'][2]
print('\n=== OCR 准确率（vs 文字层 groundtruth，CJK） ===')
for name in ['anydoc_scan', 'mineru_scan']:
    cand = cache[name][2]
    ratio = aligned_ratio(gt, cand)
    c = cer(gt, cand)
    gc = len(cjk(gt)); cc = len(cjk(cand))
    print(f"{name:<14} 总CJK={gc:<5} 产出CJK={cc:<5} CER={c*100:.2f}%  序列相似度={ratio*100:.2f}%")