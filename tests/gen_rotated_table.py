#!/usr/bin/env python3
"""生成"正文直立 + 局部旋转表"样本 PDF（表格朝向投票回归，tests/orientation.rs）。

页 1：14+4 行直立正文 + 一块 rotate(90) 画的 3×7 表格（文字层 rotation≈90°）——
默认通路下整页按单一朝向重排，旋转表被转置、正文被吞；朝向投票把它分成
{0°: 18 行, 90°: 21 项} 两组，各自摆正后分别成表/成段。
页 2：全直立页（单一朝向 → 不进分组分支），守护默认逐字节不变路径。
reportlab 内建 14 字体即可，无需外部资源。
"""
from reportlab.lib.pagesizes import A4
from reportlab.pdfgen import canvas

OUT = "tests/samples/rotated_block.pdf"

c = canvas.Canvas(OUT, pagesize=A4)
w, h = A4

# ── 页 1：直立正文 + 90° 旋转表块 ──
c.setFont("Helvetica", 11)
y = h - 80
for i in range(14):
    c.drawString(60, y, "Body line %d keeps the page upright and dominant in this file." % (i + 1))
    y -= 16

c.saveState()
c.translate(300, 300)
c.rotate(90)
c.setFont("Helvetica", 9)
cols = ["No", "Item", "Qty"]
for r in range(7):
    for ci, cname in enumerate(cols):
        t = (
            cname
            if r == 0
            else (str(r) if ci == 0 else ("part-%02d desc" % r if ci == 1 else str(r * 3)))
        )
        c.drawString(ci * 70, -r * 14, t)
c.restoreState()

y2 = 200
for i in range(4):
    c.drawString(60, y2, "Trailing body line %d after the rotated table block." % (i + 1))
    y2 -= 16
c.showPage()

# ── 页 2：全直立（单朝向 → 旧路径字节不变）──
c.setFont("Helvetica", 12)
y = h - 80
for i in range(6):
    c.drawString(60, y, "Upright only page two line %d has no rotated content at all." % (i + 1))
    y -= 20
c.save()
print(f"written {OUT}")
