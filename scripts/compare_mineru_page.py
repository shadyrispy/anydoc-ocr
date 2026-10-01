#!/usr/bin/env python3
"""单页 markdown 对照：本仓输出 vs MinerU 输出（#15，逐块 diff 详情）。

与 `compare_mineru_full.py` 的区别：本脚本是**行级** opcode 详情视图，
用于人工定位"哪几行不一致、怎么不一致"；全篇量化口径请用 full 版
（整页字符级 ratio——行级 ratio 对顺序敏感，只看差异明细时才用本脚本）。

口径：
- 两侧都去空行/注释/图片行/`---`；
- theirs 额外去 `^#+` 标题前缀（本仓标题走 `###` 前缀行，MinerU 同样有，
  但两侧级别可能差一档，去前缀后比纯文本）。

用法:
  python3 scripts/compare_mineru_page.py <mine.md> <theirs.md> [差异块数]

输出：ratio + 每个差异块（mine/theirs 各前 4 行预览）。
"""
import difflib
import sys
from pathlib import Path


def norm_md(path):
    out = []
    for ln in path.read_text(encoding="utf-8").splitlines():
        s = ln.rstrip("\n")
        st = s.strip()
        if not st or st.startswith("<!--") or st.startswith("![") or st == "---":
            continue
        out.append(s)
    return out


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        sys.exit(1)
    ra = norm_md(Path(sys.argv[1]))
    rb = norm_md(Path(sys.argv[2]))
    n_blocks = int(sys.argv[3]) if len(sys.argv) > 3 else 8

    sm = difflib.SequenceMatcher(None, ra, rb, autojunk=False)
    print(f"ratio={sm.ratio():.4f} lines mine={len(ra)} theirs={len(rb)}")
    shown = 0
    for tag, i1, i2, j1, j2 in sm.get_opcodes():
        if tag == "equal" or shown >= n_blocks:
            continue
        print(f"--- {tag} mine[{i1}:{i2}] theirs[{j1}:{j2}]")
        for ln in ra[i1:i2][:4]:
            print("  M:", ln[:110])
        for ln in rb[j1:j2][:4]:
            print("  T:", ln[:110])
        shown += 1


if __name__ == "__main__":
    main()
