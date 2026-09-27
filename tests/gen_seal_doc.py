#!/usr/bin/env python3
"""生成含真实可辨印章的**扫描型**样本 PDF（印章识别回归，tests/ocr_post.rs）。

必须是整页图像（无文字层），否则 classify 走文字层通路、OCR（及印章后处理）
根本不触发。制作环境依赖 PIL + CJK 字体；产物 .pdf 入库后测试不再依赖它们。

页 1：PIL 渲染 6 行英文正文 + "Signed:"，右下贴一枚合成红章（环排公司名 +
五角星 + "专用章"，用系统 CJK 字体渲染真字——纯色块检不出可读文本）；
页 2：纯文字扫描页（守护"无印章页不得冒出印章行"）。
"""
import glob
import math

from PIL import Image, ImageDraw, ImageFont
from reportlab.lib.pagesizes import A4
from reportlab.pdfgen import canvas

SEAL_PNG = "/tmp/seal_cn.png"
OUT = "tests/samples/seal_scan.pdf"
DPI_SCALE = 2.0  # A4 @ ~144dpi，保证章内文字可被 PP-OCR 分辨

cands = (
    glob.glob("/usr/share/fonts/**/NotoSansCJK*.ttc", recursive=True)
    + glob.glob("/usr/share/fonts/**/*CJK*.tt?", recursive=True)
    + glob.glob("/usr/share/fonts/**/wqy*.tt?", recursive=True)
)
FONT = cands[0] if cands else None
if FONT is None:
    raise SystemExit("需要 CJK 字体制作印章样本（本脚本仅在制作环境运行）")

# ── 合成印章图（白底，便于直接贴入页面）──
W = H = 400
img = Image.new("RGB", (W, H), (255, 255, 255))
cx, cy, r_out = 200, 200, 185
ImageDraw.Draw(img).ellipse(
    [cx - r_out, cy - r_out, cx + r_out, cy + r_out], outline=(200, 30, 30), width=10
)


def char(ch, ang, radius, size):
    cell = Image.new("RGBA", (70, 70), (0, 0, 0, 0))
    ImageDraw.Draw(cell).text(
        (35, 35), ch, font=ImageFont.truetype(FONT, size), fill=(200, 30, 30), anchor="mm"
    )
    x = cx + radius * math.cos(ang)
    y = cy + radius * math.sin(ang)
    cell = cell.rotate(-math.degrees(ang) - 90, expand=False, fillcolor=(0, 0, 0, 0))
    img.paste(cell, (int(x - 35), int(y - 35)), cell)


ang = -math.pi * 0.72
for ch in "北京测试科技有限公司":
    char(ch, ang, 140, 34)
    ang += 0.2
pts = []
for i in range(10):
    a = -math.pi / 2 + i * math.pi / 5
    rr = 46 if i % 2 == 0 else 18
    pts.append((cx + rr * math.cos(a), cy + rr * math.sin(a)))
ImageDraw.Draw(img).polygon(pts, fill=(200, 30, 30))
ImageDraw.Draw(img).text(
    (cx, cy + 95), "专用章", font=ImageFont.truetype(FONT, 42), fill=(200, 30, 30), anchor="mm"
)
img.save(SEAL_PNG)

# ── 渲染整页位图（扫描感：全部内容由 PIL 画进像素）──
PW, PH = int(595 * DPI_SCALE), int(841 * DPI_SCALE)


def make_page(with_seal):
    page = Image.new("RGB", (PW, PH), (255, 255, 255))
    d = ImageDraw.Draw(page)
    body = ImageFont.truetype(FONT, 26)
    y = 120
    if with_seal:
        for i in range(6):
            d.text((120, y), "Contract body line %d with no seal interference here." % (i + 1), font=body, fill=(20, 20, 20))
            y += 50
        d.text((120, y + 40), "Signed:", font=body, fill=(20, 20, 20))
        seal = Image.open(SEAL_PNG).convert("RGB").resize((420, 420))
        page.paste(seal, (int(PW * 0.55), int(PH * 0.45)))
    else:
        for i in range(6):
            d.text((120, 120 + i * 50), "Page two is plain text and gains no seal line ever.", font=body, fill=(20, 20, 20))
    return page


# ── 组 PDF：整页贴图，无文字层 ──
c = canvas.Canvas(OUT, pagesize=A4)
w, h = A4
for i, seal_page in enumerate([True, False]):
    p = f"/tmp/seal_page_{i}.png"
    make_page(seal_page).save(p)
    c.drawImage(p, 0, 0, width=w, height=h)
    c.showPage()
c.save()
print(f"written {OUT}")
