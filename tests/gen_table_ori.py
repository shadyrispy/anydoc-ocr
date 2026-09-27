#!/usr/bin/env python3
"""生成**整页图像**的旋转表样本（#8，tests/table_orientation.rs）。

为什么必须是图像页：有文字层时 classify 走文字层通路，表格结构识别与
`with_table_orientation`（#8 接的槽位）根本不触发（同 gen_wireless_tables.py）。

两个样本只差**朝向**，其余逐像素同源（同一张表 rotate 出来的）：
- `table_upright.pdf`：3×4 有线表，文字水平 → #8 的"非旋转页逐字节不变"对照组
- `table_rot90.pdf`：同一张表顺时针转 90°（文字竖排、页幅互换）→ #8 的目标组

形状约定沿用 gen_wireless_tables.py 的两条，且这里更要紧：
1. 页尺寸 = 图幅按 150dpi 反算（pt = px × 72/150），`--dpi 150` 渲染像素 1:1；
   旋转件按**旋转后**的图幅反算，保证两条通路看到的分辨率一致。
2. 画布尺寸显式给定、不随内容推算——#7 实测过 span 归属对四周留白敏感，
   而 #8 判的是"整表能否转正"，几何漂移会让结论变成噪声。

真值（转正后）：3 列 4 行、Item/Quantity/Price 表头 + Apple/Banana/Cherry 三行，
即与 `wired_table.pdf` 同一张表——所以 #8 的判据可以直接复用
`tests/wireless_table.rs` 里那套 cell 文本断言，不必另立真值。
"""
from PIL import Image, ImageDraw, ImageFont
from reportlab.pdfgen import canvas

import os

TMP = "/tmp/tori_gen"
os.makedirs(TMP, exist_ok=True)
try:
    FONT = ImageFont.truetype("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", 24)
    FONT_B = ImageFont.truetype("/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf", 24)
except OSError:
    FONT = FONT_B = ImageFont.load_default()

TARGET_DPI = 150.0

# 与 wired_table.pdf 同一张表（3 列 4 行、有框线）
COLS_W = [200, 150, 200]
ROW_H = 40
X0, Y0 = 75, 60
CELLS = [
    [("Item", True), ("Quantity", True), ("Price", True)],
    [("Apple", False), ("5", False), ("$2.50", False)],
    [("Banana", False), ("10", False), ("$1.00", False)],
    [("Cherry", False), ("20", False), ("$5.00", False)],
]


def table_bitmap(size=(800, 350)):
    W, H = size
    img = Image.new("RGB", (W, H), "white")
    d = ImageDraw.Draw(img)
    rows = len(CELLS)
    for r in range(rows + 1):
        x = X0
        for cw in COLS_W:
            d.rectangle(
                [x, Y0 + r * ROW_H, x + cw, Y0 + (r + 1) * ROW_H],
                outline="black",
                width=1,
            )
            x += cw
    for r, row in enumerate(CELLS):
        for c, (text, bold) in enumerate(row):
            x, y = X0 + sum(COLS_W[:c]), Y0 + r * ROW_H
            w, h = COLS_W[c], ROW_H
            f = FONT_B if bold else FONT
            bb = f.getbbox(text)
            d.text(
                (x + (w - (bb[2] - bb[0])) / 2, y + (h - (bb[3] - bb[1])) / 2),
                text,
                fill="black",
                font=f,
            )
    return img


def embed(path, img, tag):
    png = os.path.join(TMP, tag + ".png")
    img.save(png)
    W, H = img.size
    pw, ph = W * 72.0 / TARGET_DPI, H * 72.0 / TARGET_DPI
    c = canvas.Canvas(path, pagesize=(pw, ph))
    c.setFillColorRGB(1, 1, 1)
    c.rect(0, 0, pw, ph, fill=1, stroke=0)
    c.drawImage(png, 0, 0, width=pw, height=ph)
    c.showPage()
    c.save()
    print(f"{path}: 图幅 {W}x{H}px -> 页面 {pw:.1f}x{ph:.1f}pt")


base = table_bitmap()
embed("tests/samples/table_upright.pdf", base, "table_upright")
# transpose=ROTATE_270 → 顺时针 90°：文字由水平变竖排、页幅 W/H 互换
embed("tests/samples/table_rot90.pdf", base.transpose(Image.ROTATE_270), "table_rot90")
