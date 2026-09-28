#!/usr/bin/env python3
"""生成跨页表格测试件 `tests/samples/cross_page_table.pdf`（#6 第 3 步用）。

**为什么需要它**：第 3 步改的是 `docir/passes/cross_page_table` 的"同列续接"分支
（被吸收的续页区块从**物理删除**改为**原位保留 + `continues_prev` 标记**）。这条分支
在入库语料里**零覆盖**：25 个入库样本逐个跑过，表格样本全是单页表；golden 清单里
唯一相关的 `tests/real_samples/crosspage_table.pdf` 是那 13 个 gitignored、本沙箱
缺失的真实样本之一。拿不含该分支的语料证明"逐字节不变"是**空跑**——结论里没有
任何被改代码的执行证据。本件补上这条覆盖，并已进 `tests/golden.rs` 清单。

**形状（3 页，纯文字层，无位图）**——正好覆盖状态机的三次转移，不多不少：
- 页 1：表头 + 3 数据行 → 挂起表起始（`pending`）。
- 页 2：**同列数** + **表头印两行** + 2 数据行 → 同列续接合并（去重腿在这里被执行，见下），
  本页区块 = 第 3 步新形态的占位块（`continues_prev = Some(true)`）。
- 页 3：正文页（无网格）→ 打断挂起、前表定格。
→ 断言面：首表页 5 行合并表（thead 完整）+ 续页占位块保留 producer 原始 grid（未去重，
  渲染层按标记跳过它）+ 正文页照旧。占位块"未被去重"这件事由单测
  `repeated_header_dropped_on_continuation` 在 IR 层钉，端到端看不到它。

**本件端到端钉什么、不钉什么（实测，别按直觉理解）**：
`reconstruct_grid` 把每页**首行**放进 `header` 槽、其余进 `rows`；`extend_table_grid` 合并时
只取 `next.rows`、**从不读 `next.header`**——所以续页最顶那行**无条件**不进合并结果，
与它是不是重复表头无关。判别实验 A/B（`mkprobe.py`（沙箱 /tmp，随会话消失））：把页 2 顶行写成
`ID2/NM2/QT2/SM2`（故意 ≠ 页 1 表头）与写成真表头，输出**逐字节相同** → 证实"顶行照丢"。
去重腿 `has_header && next.rows[0] == acc.header → skip(1)` 判的是 **rows[0]**，因此只有
**续页把表头印两行**时才可达——本件就是那两行的形态：页 2 = 表头 + 表头 + 2 数据行，
`rows[0]` 命中表头 → 去重 → 合并结果 3+2 = **5 行**；若该腿退化成纯 append 就是 6 行。
对照：只印一行表头（`dupprobe.py` 的 `R_dup1`）输出**也是 5 行**，但那 5 行来自
"顶行被 header 槽吃掉 + 纯 append"，去重腿没跑。所以**光看输出行数区分不了两条路**，
本件的 5 行来自去重腿（`Q_hdr_then_X` 探针：第 2 行换成 `Z-9` → 不命中 → 6 行且保留 Z-9，
证明该腿在真判、不是恒等于 append）。
长/短表头（`has_header` 真假）在本件输出上是**惰性**的——两档都是 5 行；仍取短表头
（ID/NM/QT/SM）让 `acc.has_header` 为真，使那条合取的相等比较真被执行。

**为什么不是更多页 / 更长页序（实测，别改回去）**：早先在此记录过两个"既有缺陷"
（≥3 网格页表头丢字、正文页正文整段消失）。**两个都不成立**——它们是我探针写坏的输出
被误读成产品缺陷，真机制只有一个，且已确证：

`pdf/text_layer.rs` 的 `strip_furniture`（跨页重复文本剔除，判页眉/页脚/水印）门槛是
`pages_needed = max(3, ceil(0.6 × 总页数))`：**同文本 + 同归一化位置**（x 中心、y 各 1% 箱）
出现在 `>= pages_needed` 个不同页 → 判家具剔除。我的合成件把**同一行字**画在**每页同一坐标**，
正好撞上这个判据——它不是网格重建或跨页表 pass 的 bug。判别实验（当前二进制 `b1f505e8…`
与第 2 步二进制 `c334106d…` 各跑一遍，**18 对输出逐字节全等** → 与第 2/3 步无关；
探针脚本 `defprobe.py` / `defprobe3.py` 在沙箱 /tmp 里，随会话消失，判据本身可复现）：
- P2：3 页全表、表头**逐页相同** → 表头最左格被当家具吃掉（`<td></td><td>NM</td>…`）。
- P1：3 页全表、表头**逐页互异** → 表头**完整**。→ 门槛是"重复"不是"页数"。
- P3：6 页、3 个表页表头相同 → `pages_needed = 4 > 3` → 表头**完整**（三个表都在）。
- K 组：3 个正文页同文重复（含表文档）→ 正文丢；同形态但每页文本互异 → 三行全留；
  整篇都是同文重复正文页（无表）→ 家具判定删空整层 → 按 `text_layer.rs:118-123`
  既有设计**回落 OCR**，文本由 OCR 重新给出（代价是多付一次 OCR）。→ "正文消失"
  同样只是家具判定的产物，不是本通路 bug。

**由此暴露的真实产品面（与本件无关，已记 BACKLOG，本件不固化它）**：真实跨页表的表头行
天然"逐页同文本同位置"，短文档（总页数 <= 5 → `pages_needed = 3`）里表头重复 3 页就会被
判成页眉/水印而丢字。这是家具剔除的固有取舍，不是第 3 步引入的，也不该由本件的快照来钉。

本件因此刻意保持：总共 3 页（`pages_needed = 3`）而**表头只重复 2 页**（页 1、页 2）→
不触发家具剔除，实测表头四格完整；页 3 正文行全文档只出现 1 次 → 同样不触发。
再加页就会让重复数撞上门槛，把家具判定的产物混进"跨页合并"的断言面。

判表靠几何（`table_grid::reconstruct_table_grid`：按 y 组行 + 行内按 x 间隙聚列 +
同列 x 对齐），与语言无关，故用 ASCII 单元格文本——避开 reportlab 内建字体没有
中文的问题。列左缘每列固定（50/190/330/470 pt）→ 同列 x 散布 0 → 严格对齐判据
（`col_tol`）必过。表格上方不写标题行：实测加了标题后同一形态的网格输出走形
（`rowspan` 错乱 / 整表不成立），机制未细查，但它与本件目的无关，避开即可。

用法：`python3 tests/gen_cross_page_table.py`。输出路径由脚本位置推导，不写死
绝对路径（早期生成脚本里的 `/workspace/anydoc-ocr` 在当前沙箱不存在）。
"""
from pathlib import Path

from reportlab.lib.pagesizes import A4
from reportlab.pdfgen import canvas

OUT = Path(__file__).resolve().parent / "samples" / "cross_page_table.pdf"

# 列左缘（pt）：每列固定，跨页一致 → 同列 x 散布 0 → 严格对齐判据必过。
COL_X = [50.0, 190.0, 330.0, 470.0]
# 表头取**短**（ID/NM/QT/SM）：让 `is_header_row` 判真（`row_avg <= 5` 且
# `body_avg > 1.8 × row_avg`：短表头 row_avg=2.00 / body_avg≈5.08 > 3.60 → 真；
# 长表头 ID/NAME/QTY/SUM row_avg=3.00 → 需 body_avg>5.40 → 假）。
# 两档端到端**都输 5 行**，所以这只是让 `extend_table_grid` 那条合取的相等比较真被执行
# （长表头在第一个合取项就短路），不是为了让去重腿可达——腿可达靠的是页 2 印两行表头。
HEADER = ["ID", "NM", "QT", "SM"]
ROW_H = 22.0
Y_TOP = 100.0
FONT = "Helvetica"
FONT_SIZE = 11

PART1 = [
    ["R-1001", "Alpha-X", "128", "6400"],
    ["R-1002", "Beta-Y", "256", "12800"],
    ["R-1003", "Gamma-Z", "512", "25600"],
]
# 页 2 的**重复表头行数 = 2**（不是 1）。这不是随手写的：`reconstruct_grid` 把该页首行
# 塞进 `header` 槽、其余进 `rows`，而 `extend_table_grid` 判的是 `next.rows[0] == acc.header`
# ——首行已被 header 槽吃掉，所以**只印一行表头的续页永远命不中这条判定**。实测对照
# （`dupprobe.py`，两二进制全等）：续页 1 行表头 → 走"直接 append"分支；续页 2 行表头
# → 走 `skip(1)` 去重分支。两者输出现都是 5 行（前者 rows 只带 2 数据行、后者 3 行去重成 2 行），
# 但只有本形态真正执行被改代码里那条腿。再加一行非表头数据（`Z-9`）时输出变 6 行且保留 Z-9
# （`q_*.pdf` 探针）——证明腿在真判、不是恒等于 append。
PART2_DUP_HEADER = True
PART2 = [
    ["R-1004", "Delta-W", "640", "32000"],
    ["R-1005", "Epsilon-V", "768", "38400"],
]
BODY = [
    "This is an ordinary body paragraph with a single column of text.",
    "It carries no grid structure at all, so the pending table from the",
    "previous page must be finalized here and must not merge further.",
]


def draw_table_page(c, rows, dup_header=False):
    """一页网格表：表头行（`dup_header` 时印两行）+ 数据行。不写页上标题——实测加了
    标题后同一形态的网格输出走形（见模块 docstring），与本件目的无关，故不要。"""
    _, h = A4
    y = h - Y_TOP
    c.setFont(FONT, FONT_SIZE)
    for _ in range(2 if dup_header else 1):
        for x, t in zip(COL_X, HEADER):
            c.drawString(x, y, t)
        y -= ROW_H
    for r in rows:
        for x, t in zip(COL_X, r):
            c.drawString(x, y, t)
        y -= ROW_H


def draw_body_page(c, lines):
    _, h = A4
    y = h - Y_TOP
    c.setFont(FONT, FONT_SIZE)
    for ln in lines:
        c.drawString(50.0, y, ln)
        y -= 18.0


def main():
    OUT.parent.mkdir(parents=True, exist_ok=True)
    # `invariant=1`：固定 CreationDate/ModDate 与文档 /ID，否则 reportlab 每次写时间戳 →
    # 同一份内容重跑就改 PDF 字节，入库的二进制件会出现"没人动过却变了"的 diff。
    c = canvas.Canvas(str(OUT), pagesize=A4, invariant=1)
    c.setTitle("cross_page_table")
    draw_table_page(c, PART1)  # 页 1：挂起表起始
    c.showPage()
    draw_table_page(c, PART2, dup_header=PART2_DUP_HEADER)  # 页 2：同列续接 → 被吸收，打标记
    c.showPage()
    draw_body_page(c, BODY)    # 页 3：正文页 → 打断挂起、前表定格
    c.showPage()
    c.save()
    print(f"wrote {OUT} (3 pages)")


if __name__ == "__main__":
    main()
