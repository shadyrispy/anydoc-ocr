#!/usr/bin/env python3
"""对比 anydoc 与 mineru 输出：正文中文字符统计 + 结构页数识别"""
import re, sys, unicodedata

def cjk_count(text):
    return len(re.findall(r'[\u4e00-\u9fff]', text))

def clean_md(path):
    """去除 markdown 标记，保留正文"""
    with open(path, encoding='utf-8') as f:
        lines = f.readlines()
    out = []
    for ln in lines:
        s = ln.rstrip('\n')
        # 跳过图片、目录点线行
        if s.startswith('![') or '........' in s:
            continue
        # 去除 md 行首标记
        s = re.sub(r'^#{1,6}\s*', '', s)
        s = re.sub(r'^\|', '', s)
        s = re.sub(r'^\s*[-*]\s+', '', s)
        s = re.sub(r'^>\s*', '', s)
        out.append(s.strip())
    return '\n'.join(out)

files = ['anydoc_text.md','anydoc_scan.md',
         'mineru_text/merged_full.md','mineru_scan/merged_full.md']
for f in files:
    raw = open(f, encoding='utf-8').read()
    clean = clean_md(f)
    print(f"=== {f} ===")
    print(f"  总行:{raw.count(chr(10))}  中文字符(raw):{cjk_count(raw)}  中文字符(clean):{cjk_count(clean)}")
    # 英文/数字
    print(f"  ASCII字母:{len(re.findall(r'[A-Za-z]', raw))}  数字:{len(re.findall(r'\d', raw))}")