#!/usr/bin/env python3
"""生成无线/有线表格结构样本（#7，tests/wireless_table.rs）。

三个**整页图像** PDF（无文字层）：若有文字层，classify 会走文字层通路，表格
结构识别（slanet_plus）根本不触发。产物入库后测试不再依赖 PIL/reportlab。

- wireless_simple.pdf：3×3 无线表（无框线）→ 真值 3 行 3 列、无 span
- wireless_span.pdf：4×3 无线表 → 真值 Header1 colspan=2、Merged rowspan=2
  **这是 #7 的判据样本**：现状通路（slanet_plus 通用兜底）全对，
  `ANYDOC_WIRELESS_CELLS`（cells→HTML）会把 colspan 撑成 3 并丢 Data3/Data5/Data7
- wired_table.pdf：4×3 有线表 → 回归用，守"开 A/B 开关时有线表逐字节不变"

两处刻意的形状约定（不是随便画的）：
1. **页尺寸 = 图幅按 150dpi 反算**（pt = px × 72/150），使 `--dpi 150` 渲染时
   像素逐点 1:1。若按固定 A4 等比贴满，图会被缩到 0.3 倍（24pt 字 → ~22px），
   测的就不是同一条检测尺度。
2. **画布高/宽显式给定、不靠内容推算**：实测表格块四周留白比例会影响 span 落位
   （同一段代码两种 padding 给出两种 rowspan 归属）。留白是样本的一部分，
   所以钉死，别让它随字号漂移。
"""
from PIL import Image, ImageDraw, ImageFont
from reportlab.pdfgen import canvas

import os

TMP = "/tmp/wtab_gen"
os.makedirs(TMP, exist_ok=True)
try:
    FONT = ImageFont.truetype("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", 24)
    FONT_B = ImageFont.truetype("/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf", 24)
except OSError:
    FONT = FONT_B = ImageFont.load_default()

TARGET_DPI = 150.0  # 页尺寸反算基准，与测试里的 --dpi 一致


def grid_pdf(path, size, x0, y0, cols_w, row_h, cells, borders=False):
    """cells = [(row, col, text, rowspan, colspan, bold)]，坐标单位为逻辑 px。

    size = 位图 (W, H)，显式给定（含四周留白）；文本在单元格区域内居中。
    """
    W, H = size
    img = Image.new("RGB", (W, H), "white")
    d = ImageDraw.Draw(img)
    if borders:
        rows = max(r + rs for r, _, _, rs, _, _ in cells)
        for r in range(rows + 1):
            x = x0
            for cw in cols_w:
                d.rectangle(
                    [x, y0 + r * row_h, x + cw, y0 + (r + 1) * row_h],
                    outline="black",
                    width=1,
                )
                x += cw
    for r, c, text, rs, cs, bold in cells:
        x, y = x0 + sum(cols_w[:c]), y0 + r * row_h
        w, h = sum(cols_w[c : c + cs]), rs * row_h
        f = FONT_B if bold else FONT
        bb = f.getbbox(text)
        d.text(
            (x + (w - (bb[2] - bb[0])) / 2, y + (h - (bb[3] - bb[1])) / 2),
            text,
            fill="black",
            font=f,
        )
    png = os.path.join(TMP, os.path.basename(path).replace(".pdf", ".png"))
    img.save(png)
    pw, ph = W * 72.0 / TARGET_DPI, H * 72.0 / TARGET_DPI
    c = canvas.Canvas(path, pagesize=(pw, ph))
    c.setFillColorRGB(1, 1, 1)
    c.rect(0, 0, pw, ph, fill=1, stroke=0)
    c.drawImage(png, 0, 0, width=pw, height=ph)
    c.showPage()
    c.save()


grid_pdf(
    "tests/samples/wireless_simple.pdf",
    (800, 350),
    75,
    60,
    [200, 150, 200],
    40,
    [
        (0, 0, "Name", 1, 1, True),
        (0, 1, "Age", 1, 1, True),
        (0, 2, "City", 1, 1, True),
        (1, 0, "Alice", 1, 1, False),
        (1, 1, "28", 1, 1, False),
        (1, 2, "Beijing", 1, 1, False),
        (2, 0, "Bob", 1, 1, False),
        (2, 1, "35", 1, 1, False),
        (2, 2, "Shanghai", 1, 1, False),
    ],
)

grid_pdf(
    "tests/samples/wireless_span.pdf",
    (900, 540),
    75,
    60,
    [200, 200, 200],
    60,
    [
        (0, 0, "Header1", 1, 2, True),
        (0, 2, "Header2", 1, 1, True),
        (1, 0, "Data1", 1, 1, False),
        (1, 1, "Data2", 1, 1, False),
        (1, 2, "Data3", 1, 1, False),
        (2, 0, "Merged", 2, 1, False),
        (2, 1, "Data4", 1, 1, False),
        (2, 2, "Data5", 1, 1, False),
        (3, 1, "Data6", 1, 1, False),
        (3, 2, "Data7", 1, 1, False),
    ],
)

grid_pdf(
    "tests/samples/wired_table.pdf",
    (800, 350),
    75,
    60,
    [200, 150, 200],
    40,
    [
        (0, 0, "Item", 1, 1, True),
        (0, 1, "Quantity", 1, 1, True),
        (0, 2, "Price", 1, 1, True),
        (1, 0, "Apple", 1, 1, False),
        (1, 1, "5", 1, 1, False),
        (1, 2, "$2.50", 1, 1, False),
        (2, 0, "Banana", 1, 1, False),
        (2, 1, "10", 1, 1, False),
        (2, 2, "$1.00", 1, 1, False),
        (3, 0, "Cherry", 1, 1, False),
        (3, 1, "20", 1, 1, False),
        (3, 2, "$5.00", 1, 1, False),
    ],
    borders=True,
)

print("已生成 tests/samples/{wireless_simple,wireless_span,wired_table}.pdf")
