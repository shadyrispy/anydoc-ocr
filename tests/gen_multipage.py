#!/usr/bin/env python3
"""生成多页图片型 PDF（无文本层，纯位图烧录中文），用于验证 --threads 页级并行加速。"""
from PIL import Image, ImageDraw, ImageFont

FONT = "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"
OUT = "/workspace/anydoc-ocr/tests/samples/multipage.pdf"
PAGES = 8
W, H = 900, 1273  # A4 近似 @108dpi


def make_page(p):
    img = Image.new("RGB", (W, H), "white")
    d = ImageDraw.Draw(img)
    font = ImageFont.truetype(FONT, 36, index=0)
    for i in range(12):
        y = 90 + i * 95
        d.text((70, y), f"第{p+1}页 第{i+1}行 任意文档识别测试 OCR 性能验证", fill="black", font=font)
    return img


pages = [make_page(p) for p in range(PAGES)]
pages[0].save(OUT, "PDF", save_all=True, append_images=pages[1:], resolution=108.0)
print(f"wrote {OUT} ({PAGES} pages)")
