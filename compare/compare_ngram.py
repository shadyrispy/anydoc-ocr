#!/usr/bin/env python3
"""字符级识别准确率（顺序/切分鲁棒）：unigram/bigram 召回。
unigram 召回=GT字符是否被识别；bigram 召回=相邻字符对是否保留（测顺序/切分）。
"""
import re
from collections import Counter

def cjk(s): return re.findall(r'[\u4e00-\u9fff]', s)

def ngrams(s, n):
    return Counter(s[i:i+n] for i in range(len(s)-n+1))

def load(path):
    buf = []
    for ln in open(path, encoding='utf-8'):
        s = ln.rstrip('\n')
        if s.startswith('!['): continue
        s = re.sub(r'^#{1,6}\s*', '', s)
        s = re.sub(r'^\|', '', s); s = re.sub(r'\|$', '', s)
        s = re.sub(r'^\s*[-*]\s+', '', s); s = re.sub(r'^>\s*', '', s)
        s = s.strip()
        if not s: continue
        if re.fullmatch(r'\d{1,3}', s): continue
        if 'GJB 9001C' in s: continue
        if '........' in s: continue
        if set(s) <= set('|:- \t') and s.count('|') >= 2: continue
        buf.append(s)
    return ''.join(buf)

gt = ''.join(cjk(load('anydoc_text.md')))
print(f'GT CJK 字符: {len(gt)}')
for name, f in [('anydoc_scan','anydoc_scan.md'),
                ('mineru_scan','mineru_scan/merged_full.md')]:
    ocr = ''.join(cjk(load(f)))
    gu, ou = set(gt), set(ocr)
    g2, o2 = set(ngrams(gt,2)), set(ngrams(ocr,2))
    g3, o3 = set(ngrams(gt,3)), set(ngrams(ocr,3))
    unigram_recall = len(gu & ou)/len(gu)*100
    bigram_recall = len(g2 & o2)/len(g2)*100
    trigram_recall = len(g3 & o3)/len(g3)*100
    # 字符总量偏差（漏识/误识/重复）
    print(f'\n=== {name} ===  产出CJK={len(ocr)}')
    print(f'  unigram召回(字符识别) = {unigram_recall:.2f}%')
    print(f'  bigram召回(双字顺序)  = {bigram_recall:.2f}%')
    print(f'  trigram召回(三字)     = {trigram_recall:.2f}%')