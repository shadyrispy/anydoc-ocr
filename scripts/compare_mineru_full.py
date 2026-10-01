#!/usr/bin/env python3
"""全篇逐页对照：本仓 content_list v2 输出 vs MinerU 按页 markdown（#15）。

口径（BACKLOG #15 记录以本脚本为准）：
- mine：`anydoc-ocr <pdf> --format content-list-v2 -o <json>` 的产物。
  list of pages，每页 item 为 {bbox, content, type}；按 type 展开
  title/paragraph/page_header/footer/number/footnote → `*_content[].content`，
  index/list → `list_items[].item_content[].content`，table → `content.html`
  去标签。furniture 与正文同权参与比对。
- theirs：MinerU 输出目录下每页一个 md（`<dir>/<页号>.md`）。去空行/注释/
  图片行/`---` 分隔、`^#+` 标题前缀、pipe 表格线、内联 HTML 标签。
- 每页两侧各 join 成整页字符串，`SequenceMatcher(autojunk=False)` **字符级**
  ratio（行级对顺序敏感，目次/页眉顺序差异会压低 ratio——#15 实测教训）。
- autojunk 必须显式关闭：默认 True 会把长中文页的高频汉字判 junk，
  ratio 崩塌成假信号（页 22 曾虚假 0.015）。

用法:
  python3 scripts/compare_mineru_full.py <mine_cl_v2.json> <theirs_pages_dir> [总页数]

输出：avg ratio、<0.85 页列表（降序）、全部页 ratio。
"""
import html
import json
import re
import sys
from difflib import SequenceMatcher
from pathlib import Path

CONTENT_KEYS = (
    "title_content",
    "paragraph_content",
    "page_header_content",
    "page_footer_content",
    "page_number_content",
    "footnote_content",
)

THRESHOLD = 0.85


def segs_of(item):
    """一个 content_list v2 item → 文本段列表。"""
    t = item.get("type")
    c = item.get("content")
    out = []
    if not isinstance(c, dict):
        if isinstance(c, str) and c.strip():
            out.append(c)
        return out
    if t in ("index", "list"):
        for x in c.get("list_items", []):
            ic = x.get("item_content", [])
            if isinstance(ic, list):
                for y in ic:
                    s = y.get("content", "") if isinstance(y, dict) else str(y)
                    if s.strip():
                        out.append(s)
            elif isinstance(ic, str) and ic.strip():
                out.append(ic)
        return out
    if t == "table":
        h = c.get("html", "")
        h = re.sub(r"<[^>]+>", " ", h)
        h = html.unescape(h)
        s = re.sub(r"\s+", " ", h).strip()
        if s:
            out.append(s)
        return out
    for k in CONTENT_KEYS:
        for x in c.get(k, []):
            s = x.get("content", "") if isinstance(x, dict) else str(x)
            if s.strip():
                out.append(s)
    return out


def mine_page(items):
    lines = []
    for it in items:
        if it.get("type") == "image":
            continue
        for seg in segs_of(it):
            lines.append(seg.strip())
    return lines


def theirs_page(path):
    lines = []
    for ln in path.read_text(encoding="utf-8").splitlines():
        s = ln.rstrip("\n")
        st = s.strip()
        if not st or st.startswith("<!--") or st.startswith("![") or st == "---":
            continue
        if st.startswith("#"):
            s = re.sub(r"^#+\s*", "", s)
            st = s.strip()
        if st.startswith("|"):
            s = st.strip("|").replace("|", " ")
        s = re.sub(r"<[^>]+>", " ", s)
        s = re.sub(r"\s+", " ", s).strip()
        if s:
            lines.append(s)
    return lines


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        sys.exit(1)
    mine_path = Path(sys.argv[1])
    theirs_dir = Path(sys.argv[2])
    n_pages = int(sys.argv[3]) if len(sys.argv) > 3 else None

    mine = json.loads(mine_path.read_text(encoding="utf-8"))
    if n_pages is None:
        n_pages = len(mine)

    ratios = {}
    for pg in range(1, n_pages + 1):
        a = mine_page(mine[pg - 1]) if pg - 1 < len(mine) else []
        tp = theirs_dir / f"{pg}.md"
        b = theirs_page(tp) if tp.exists() else []
        sa, sb = "\n".join(a), "\n".join(b)
        ratios[pg] = SequenceMatcher(None, sa, sb, autojunk=False).ratio()

    avg = sum(ratios.values()) / len(ratios)
    print(f"pages={len(ratios)}  avg ratio = {avg:.4f}")
    low = [(p, round(r, 3)) for p, r in sorted(ratios.items(), key=lambda kv: kv[1]) if r < THRESHOLD]
    print(f"pages <{THRESHOLD}:", low if low else "none")
    print("all:", {p: round(r, 3) for p, r in sorted(ratios.items())})


if __name__ == "__main__":
    main()
