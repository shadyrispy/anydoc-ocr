#!/usr/bin/env python3
"""生成真实感全页带框表格图片 PDF（验证表格识别方案用）。"""
from PIL import Image, ImageDraw, ImageFont

FONT = "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"
OUT = "/workspace/anydoc-ocr/tests/samples/real_table.pdf"
W, H = 1240, 1754  # A4 @150dpi

img = Image.new("RGB", (W, H), "white")
d = ImageDraw.Draw(img)
font = ImageFont.truetype(FONT, 34, index=0)
font_b = ImageFont.truetype(FONT, 40, index=0)

d.text((80, 60), "月度销售统计表", fill="black", font=font_b)
d.rectangle((80, 140, W - 80, H - 200), outline="black", width=4)

# 表头
headers = ["月份", "产品A", "产品B", "产品C", "合计"]
col_x = [80, 300, 520, 740, 960]
rows = [
    ["一月", "120", "340", "210", "670"],
    ["二月", "150", "300", "240", "690"],
    ["三月", "180", "280", "260", "720"],
    ["四月", "160", "330", "230", "720"],
    ["五月", "200", "310", "250", "760"],
    ["六月", "220", "290", "270", "780"],
]

# 表头行
d.rectangle((80, 140, W - 80, 240), outline="black", width=2)
for x, h in zip(col_x, headers):
    d.text((x + 20, 160), h, fill="black", font=font_b)

y0 = 240
rh = 200
for r, row in enumerate(rows):
    top = y0 + r * rh
    d.rectangle((80, top, W - 80, top + rh), outline="black", width=2)
    for x, cell in zip(col_x, row):
        d.text((x + 20, top + 40), cell, fill="black", font=font)

img.save(OUT, "PDF", resolution=150.0)
print("wrote", OUT)
