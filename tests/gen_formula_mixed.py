#!/usr/bin/env python3
"""#9 第 0 步测试件：**图像页**上同时含 行内公式 / 带编号行间公式 / 无编号行间公式 /
表内公式 / 表内图 五个观测点。

为什么必须是图像页（同 `gen_table_ori.py`）：有文字层时 classify 走文字层通路，
版面模型与公式识别（MFD/MFR）根本不触发——而那正是本票要量的东西。matplotlib
直接存 PDF 会带**真文字层**（Type-42 字形），所以先存 **PNG**（150dpi），再用
reportlab 作为唯一 XObject 嵌进 PDF；页尺寸按图幅反算（pt = px×72/150），
`--dpi 150` 渲染回像素 1:1（#7/#8 的教训：几何漂移会让结论变成噪声）。

第一版踩过的两个坑（都在这版里修掉，注释标注位置）：
- **文本块相互重叠**：手写的 x 偏移让公式压住了 "The relation**n**" 和
  "**c**onnects"，det/rec 双侧都读出残字 → 量到的"掉行"混了排版伪影。现在行内
  公式的左右两段文字用 **renderer 实测宽度**顺排，留 8px 间隙。
- **表内图 axes 落错行**：`fig.add_axes` 用的是**归一化 + 左下角原点**，直接拿
  数据坐标算 y 会把曲线画到表格外（第一版画到了尾行文字上，两家都吞了半行）。
  现在用 `cell_band()` 显式换算。

真值（人写死，供两侧输出对照）：
1. 行内：`The relation $E=mc^2$ links mass and energy here.`
2. 行间带编号：`V = IR` + 右侧 `(1)`
3. 无编号行间：`∫₀^∞ e^{-x²} dx = √π / 2`
4. 表内公式：`f(x) = x² + 1`（第二行第二格）
5. 表内图：一块灰度渐变图（第二行第三格）
"""
import os

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
import numpy as np
from reportlab.pdfgen import canvas

TMP = "/tmp/fx_gen"
os.makedirs(TMP, exist_ok=True)

TARGET_DPI = 150.0
W_PX, H_PX = 1000, 620  # → 480.0 x 297.6 pt
INK = 15

# 表格几何（数据坐标，y 轴向上）
COLS = 3
CW = 175
RH = 48
TBL_X = INK
TBL_TOP = 400  # 表头顶边（header 行在 [TBL_TOP-RH, TBL_TOP]）


def cell_band(row):
    """第 row 行（0=表头）的 data-y 区间 (bottom, top)。"""
    top = TBL_TOP - row * RH
    return top - RH, top


def place(ax, fig, y, pieces, size=13):
    """把 [(text, is_math)] 顺排成一行，返回下一个可用 x。

    用 renderer 实测宽度定位，杜绝手调偏移导致的重叠（第一版的坑）。
    """
    fig.canvas.draw()
    rend = fig.canvas.get_renderer()
    x = INK
    for text, _is_math in pieces:
        probe = ax.text(x, y, text, fontsize=size, va="top", ha="left")
        w = probe.get_window_extent(rend).width
        probe.remove()
        ax.text(x, y, text, fontsize=size, va="top", ha="left")
        x += w + 8
    return x


def build(page_pdf, png_path):
    fig = plt.figure(figsize=(W_PX / 150.0, H_PX / 150.0), dpi=150)
    ax = fig.add_axes([0, 0, 1, 1])
    ax.set_xlim(0, W_PX)
    ax.set_ylim(0, H_PX)
    ax.axis("off")

    # 1) 正文夹行内公式（三段顺排，互不重叠）
    place(ax, fig, 578, [("The relation", False), (r"$E = mc^2$", True),
                         ("links mass and energy here.", False)])

    # 2) 行间公式 + 右侧编号
    ax.text(150, 528, r"$V = IR$", fontsize=16, va="top", ha="left")
    ax.text(880, 528, "(1)", fontsize=13, va="top", ha="left")

    # 3) 无编号行间公式
    ax.text(150, 470, r"$\int_0^{\infty} e^{-x^2}\,dx = \frac{\sqrt{\pi}}{2}$",
            fontsize=16, va="top", ha="left")

    # 4)+5) 表内公式 / 表内图
    for r in range(3):  # 3 条横线（表头上下 + 数据行下）
        y = TBL_TOP - r * RH
        ax.plot([TBL_X, TBL_X + CW * COLS], [y, y], lw=1.2, c="black")
    for c in range(COLS + 1):
        ax.plot([TBL_X + c * CW, TBL_X + c * CW],
                [TBL_TOP - 2 * RH, TBL_TOP], lw=1.2, c="black")

    b0, t0 = cell_band(0)
    b1, t1 = cell_band(1)
    ax.text(TBL_X + 10, t0 - 14, "Symbol", fontsize=12, va="center", ha="left")
    ax.text(TBL_X + CW + 10, t0 - 14, "Definition", fontsize=12, va="center", ha="left")
    ax.text(TBL_X + CW * 2 + 10, t0 - 14, "Plot", fontsize=12, va="center", ha="left")
    ax.text(TBL_X + 10, t1 - 14, "f", fontsize=12, va="center", ha="left")
    ax.text(TBL_X + CW + 10, t1 - 14, r"$f(x) = x^2 + 1$", fontsize=12,
            va="center", ha="left")
    # 表内图：落在数据行第三格内（归一化坐标 + 左下原点，显式换算）。
    # 注意第一版画的是细 sine 曲线（120x24 白底黑线）——两家版面模型都把它判成
    # `inline_formula`（我们 dump 里是 Formula/inline_formula，MinerU 直接输出
    # `$\smile$`），于是"表内**图**"这一格根本没被量到。改成灰度渐变块（有实际
    # 墨量、非符号形状），让 Image 分类有机会触发。
    gx, gy = TBL_X + CW * 2 + 20, b1 + 8
    ax2 = fig.add_axes([gx / W_PX, gy / H_PX, 135 / W_PX, 32 / H_PX])
    yy, xx = np.mgrid[0:32, 0:135]
    patch = (np.sin(xx / 18.0) * 40 + yy * 3.0 + 90.0).clip(0, 255).astype(np.uint8)
    ax2.imshow(patch, cmap="gray", aspect="auto")
    ax2.set_xticks([])
    ax2.set_yticks([])
    for s in ax2.spines.values():
        s.set_linewidth(0.8)

    # 收尾正文（离表 ≥40px，验证公式/表块之后正文正常接续）
    ax.text(INK, 215, "Paragraph continues after the equations above.", fontsize=13,
            va="top", ha="left")

    fig.savefig(png_path, dpi=TARGET_DPI)
    plt.close(fig)

    from PIL import Image
    W, H = Image.open(png_path).size
    pw, ph = W * 72.0 / TARGET_DPI, H * 72.0 / TARGET_DPI
    c = canvas.Canvas(page_pdf, pagesize=(pw, ph))
    c.setFillColorRGB(1, 1, 1)
    c.rect(0, 0, pw, ph, fill=1, stroke=0)
    c.drawImage(png_path, 0, 0, width=pw, height=ph)
    c.showPage()
    c.save()
    print(f"{page_pdf}: 图幅 {W}x{H}px -> 页面 {pw:.1f}x{ph:.1f}pt")


os.makedirs("tests/samples", exist_ok=True)
build("tests/samples/formula_mixed.pdf", os.path.join(TMP, "formula_mixed.png"))
