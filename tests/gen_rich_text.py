#!/usr/bin/env python3
"""行内样式样本 PDF（tests/pages_rich_text.rs 用）。

原用途：`ANYDOC_RICH_TEXT` 开启时验证 `**…**`/`*…*` 注入与标题前缀共存。
该变量已废弃（#6 决策 (c)：行为移除、命中只告警），本样本**保留**——它是仓内
唯一带 bold/italic 字体证据的文字层 PDF，现在用于钉反面契约：文字层 producer
不得再把样式注入成正文字面量，且设与不设输出逐字节相同。
单页：加粗编号标题、普通正文（对照组）、Helvetica-Bold 行、Helvetica-Oblique
行、无样式尾行。reportlab 内建 14 字体即可，无需外部资源。
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
