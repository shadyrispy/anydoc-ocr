#!/usr/bin/env python3
"""生成行内样式样本 PDF（ANYDOC_RICH_TEXT 回归用，tests/pages_rich_text.rs）。

单页：加粗编号标题（验证 `## **…**` 前缀与样式共存）、普通正文（对照组）、
Helvetica-Bold 行（`**…**`）、Helvetica-Oblique 行（`*…*`）、无样式尾行。
reportlab 内建 14 字体即可，无需外部资源。
"""
from reportlab.pdfgen import canvas
from reportlab.lib.pagesizes import A4

OUT = "tests/samples/rich_text.pdf"

c = canvas.Canvas(OUT, pagesize=A4)
w, h = A4
y = h - 90
c.setFont("Helvetica-Bold", 16)
c.drawString(72, y, "1. General Rules")
y -= 40
c.setFont("Helvetica", 12)
c.drawString(72, y, "Regular body sentence for control.")
y -= 30
c.setFont("Helvetica-Bold", 12)
c.drawString(72, y, "Bold lead-in text")
y -= 30
c.setFont("Helvetica-Oblique", 12)
c.drawString(72, y, "Italic styled text")
y -= 30
c.setFont("Helvetica", 12)
c.drawString(72, y, "Plain tail line.")
c.showPage()
c.save()
print(f"written {OUT}")
