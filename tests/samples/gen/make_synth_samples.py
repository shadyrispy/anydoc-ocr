#!/usr/bin/env python3.11
"""合成测试样本生成器 —— ground truth 由生成过程本身给出。

每页对应 BACKLOG 的一个待决条目：
  页1 chart     #10 chart 块类型（探针：pp-doclayoutv2 判不判 CHART）
  页2 表内图    #9 修法 5（版面判 Image 且落在表格 bbox 内）
  页3 紧行高表  #7 ANYDOC_WIRELESS_CELLS 复测语料
  页4 弧形印章  #5a ANYDOC_SEAL_ARC 默认值待重议

原则：图内文字/数值全部由本脚本写死，ground truth 即源码常量。
可复现：重跑本脚本得到逐字节等价输入（matplotlib 关闭 hash 随机性）。
"""
import json
import math
import pathlib

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
from PIL import Image, ImageDraw, ImageFont
from weasyprint import CSS, HTML

plt.rcParams["font.sans-serif"] = ["Noto Sans CJK JP"]
plt.rcParams["axes.unicode_minus"] = False

ROOT = pathlib.Path(__file__).resolve().parents[1]          # tests/samples
ASSETS = ROOT / "synth_assets"
PDF = ROOT / "synth_samples.pdf"
GT = ROOT / "synth_samples_gt.json"

# ---------------- 资产：图表 ----------------

CHART_LABELS = {
    "title": "图 3-1 分季度营业收入与成本对比",
    "xlabel": "报告期",
    "ylabel": "金额（亿元）",
    "legend": ["营业收入", "营业成本"],
    "ticks": ["第一季度", "第二季度", "第三季度", "第四季度"],
    "values": ["128.5", "142.3", "156.8", "171.2", "96.2", "104.5", "112.1", "118.9"],
}


def make_bar_chart(path: pathlib.Path) -> None:
    """页1 资产：文字密集的柱状图（图例+轴标签+数据标签，测图内 OCR 归属）。"""
    rev = [128.5, 142.3, 156.8, 171.2]
    cost = [96.2, 104.5, 112.1, 118.9]
    q = CHART_LABELS["ticks"]
    fig, ax = plt.subplots(figsize=(6.2, 3.4), dpi=200)
    x = range(len(q))
    b1 = ax.bar([i - 0.2 for i in x], rev, 0.4, label=CHART_LABELS["legend"][0], color="#2f6f9f")
    b2 = ax.bar([i + 0.2 for i in x], cost, 0.4, label=CHART_LABELS["legend"][1], color="#c96a3f")
    for bars in (b1, b2):
        for r in bars:
            ax.annotate(f"{r.get_height():.1f}",
                        (r.get_x() + r.get_width() / 2, r.get_height()),
                        ha="center", va="bottom", fontsize=6.5)
    ax.set_xticks(list(x))
    ax.set_xticklabels(q, fontsize=7.5)
    ax.set_xlabel(CHART_LABELS["xlabel"], fontsize=8)
    ax.set_ylabel(CHART_LABELS["ylabel"], fontsize=8)
    ax.set_title(CHART_LABELS["title"], fontsize=9.5)
    ax.legend(fontsize=7.5, loc="upper left", framealpha=0.9)
    ax.grid(axis="y", alpha=0.3, linewidth=0.5)
    fig.tight_layout()
    fig.savefig(path)
    plt.close(fig)


def make_sparkline(path: pathlib.Path) -> None:
    """页2 资产：单元格内折线图（表内图判据的触发件）。

    **边框是可达性的决定因素**（实测单变量对照：同为 60×35mm，仅差一个 2pt
    深色矩形框 —— 有框 → 版面吐出独立 `image` 且 bbox ⊆ table；无框 → 零
    image 元素）。原 v1 样本 32mm + 透明底 + 无框，三要素全在失败侧，判据
    从未被触发。四项缺一不可：物理尺寸 55×26mm、不透明白底、**显式
    Rectangle 边框**（`axis("off")` 连 spines 一起不画，必须用 Rectangle
    叠 zorder）、深色 `#0f2f47` 2.0pt。图内**刻意不写字**——有文字会把
    判定推向 `chart` 分支而丢失 `image` 归类。

    复现证据：/tmp/tableimg_exp/v1.pdf 过本仓 OCR 通路 →
    `image conf=0.574 box=(464,276)-(685,371)`，inside table = True。
    """
    import numpy as np
    from matplotlib.patches import Rectangle

    MM = 1 / 25.4  # pt per mm → figsize 单位是 inch
    xs = np.linspace(0, 4 * math.pi, 120)
    ys = np.sin(xs) * 0.6 + 0.3
    fig, ax = plt.subplots(figsize=(55 * MM, 26 * MM), dpi=300)
    ax.plot(xs, ys, color="#14496b", linewidth=1.6)
    ax.set_xticks([])
    ax.set_yticks([])
    ax.add_patch(Rectangle((0, 0), 1, 1, transform=ax.transAxes, facecolor="none",
                           edgecolor="#0f2f47", linewidth=2.0, zorder=5))
    fig.savefig(path, facecolor="white")   # 不透明白底
    plt.close(fig)


SEAL_ARC_TEXT = "江市市场监督管理局"


def make_seal(path: pathlib.Path) -> None:
    """页4 资产：章顶环排文字图章（#5a），五角星 + 底部横排。"""
    S = 900
    img = Image.new("RGBA", (S, S), (255, 255, 255, 0))
    d = ImageDraw.Draw(img)
    red = (200, 30, 38, 255)
    d.ellipse([28, 28, S - 28, S - 28], outline=red, width=14)
    try:
        f = ImageFont.truetype("/usr/share/fonts/opentype/noto/NotoSerifCJK-Bold.ttc", 58)
        fs = ImageFont.truetype("/usr/share/fonts/opentype/noto/NotoSerifCJK-Bold.ttc", 52)
    except OSError:
        f = fs = ImageFont.load_default()
    # 环排：上弧 180°→0°，字符沿圆周均匀分布
    cx = cy = S / 2
    r = S / 2 - 88
    n = len(SEAL_ARC_TEXT)
    for i, ch in enumerate(SEAL_ARC_TEXT):
        ang = math.pi * (1 - (i + 0.5) / n)
        ax_, ay_ = cx + r * math.cos(ang), cy + r * math.sin(ang)
        bb = d.textbbox((0, 0), ch, font=f)
        w, h = bb[2] - bb[0], bb[3] - bb[1]
        rot = -math.degrees(ang) + 90
        pad = 8
        tile = Image.new("RGBA", (w + pad * 2, h + pad * 2), (255, 255, 255, 0))
        ImageDraw.Draw(tile).text((pad - bb[0], pad - bb[1]), ch, font=f, fill=red)
        tile = tile.rotate(rot, resample=Image.BICUBIC, expand=True)
        img.alpha_composite(tile, (int(ax_ - tile.width / 2), int(ay_ - tile.height / 2)))
    # 中心五角星
    cx0, cy0, R = cx, cy + 40, 105
    pts = []
    for i in range(10):
        rr = R if i % 2 == 0 else R * 0.42
        a = -math.pi / 2 + i * math.pi / 5
        pts.append((cx0 + rr * math.cos(a), cy0 + rr * math.sin(a)))
    d.polygon(pts, fill=red)
    # 底部横排
    bottom = "档案专用"
    bb = d.textbbox((0, 0), bottom, font=fs)
    d.text((cx - (bb[2] - bb[0]) / 2, cy + 168), bottom, font=fs, fill=red)
    img.save(path)


# ---------------- 页面 ----------------

BODY_CSS = """
@page { size: A4; margin: 20mm 18mm; }
body { font-family: "Noto Serif CJK JP", serif; font-size: 10.5pt; line-height: 1.75; color:#111; }
h1 { font-size: 15pt; margin: 0 0 10pt; }
h2 { font-size: 12pt; margin: 14pt 0 6pt; }
p  { margin: 0 0 8pt; text-align: justify; }
.cap { font-size: 8.5pt; text-align: center; color:#333; margin: 4pt 0 12pt; }
img.chart { display:block; width: 150mm; margin: 6pt auto; }
table { border-collapse: collapse; width: 100%; margin: 6pt 0; }
th, td { padding: 4pt 6pt; }
thead th { border-bottom: 1.2pt solid #222; font-weight: 600; }
tbody td { border-bottom: 0.4pt solid #bbb; }
table.wireless tbody td { border-bottom: none; }
table.tight { font-size: 8.2pt; line-height: 1.0; }
table.tight th, table.tight td { padding: 0.5pt 3pt; }
img.spark { display:block; width: 55mm; }
.seal { position: absolute; top: 24mm; right: 16mm; width: 38mm; }
.rel { position: relative; page-break-after: always; }
"""

P1 = f"""
<div class="rel"><img class="seal" src="{(ASSETS / 'seal.png').as_uri()}">
<h1>第三章 经营情况分析</h1>
<h2>3.1 分季度营收对比</h2>
<p>报告期内，公司营业收入连续四个季度保持增长态势，累计实现营业收入 598.8 亿元，
较上一年度增长 14.2%。其中第四季度营业收入达到 171.2 亿元，创历史单季新高，
主要受益于华东区域渠道拓展与新产品线放量。</p>
<p>从成本结构看，各季度营业成本占营业收入比重稳定在 69% 至 75% 区间，
毛利率未出现显著波动。管理层认为当前成本管控总体有效，
后续将通过对供应链环节的进一步整合巩固现有毛利率水平。</p>
<img class="chart" src="{(ASSETS / 'chart_bar.png').as_uri()}">
<p class="cap">图 3-1 分季度营业收入与成本对比　数据来源：内部财务台账</p>
</div>
"""

P2 = f"""
<div class="rel">
<h1>附表：主要产品线产能利用率</h1>
<p>下表列示各产品线近三个季度的产能利用率。其中第二季度环比趋势以曲线图形式给出，
便于观察产能爬坡节奏。</p>
<table>
<thead><tr><th>产品线</th><th>2024Q2</th><th>2024Q3</th><th>趋势</th></tr></thead>
<tbody>
<tr><td>智能终端</td><td>72.4%</td><td>78.1%</td><td></td></tr>
<tr><td>工业模组</td><td>64.9%</td><td>69.3%</td><td><img class="spark" src="{(ASSETS / 'spark.png').as_uri()}"></td></tr>
<tr><td>车载电子</td><td>81.2%</td><td>83.5%</td><td>—</td></tr>
<tr><td>新能源组件</td><td>58.7%</td><td>66.4%</td><td>↑</td></tr>
</tbody>
</table>
</div>
"""

TIGHT_ROWS = [
    ("华东", "1,284", "936", "1,082", "77.1", "+5.2"),
    ("华南", "968", "702", "815", "74.8", "+3.1"),
    ("华北", "1,037", "758", "869", "72.3", "-1.4"),
    ("华中", "712", "549", "604", "73.6", "+0.8"),
    ("西南", "586", "441", "512", "74.1", "+2.2"),
    ("东北", "334", "262", "271", "76.9", "-0.6"),
    ("西北", "287", "221", "233", "77.4", "+1.1"),
    ("合计", "5,208", "3,869", "4,386", "75.4", "+2.6"),
]
_tbody = "".join(
    f"<tr><td>{r[0]}</td><td>{r[1]}</td><td>{r[2]}</td><td>{r[3]}</td><td>{r[4]}%</td><td>{r[5]}</td></tr>"
    for r in TIGHT_ROWS
)
P3 = f"""
<div class="rel">
<h1>附表：分区域销售明细（单位：万元）</h1>
<table class="wireless tight">
<thead><tr><th>区域</th><th>营业收入</th><th>营业成本</th><th>毛利</th><th>毛利率</th><th>同比</th></tr></thead>
<tbody>{_tbody}</tbody>
</table>
</div>
"""

P4 = f"""
<div class="rel"><img class="seal" src="{(ASSETS / 'seal.png').as_uri()}">
<h1>附：测算说明</h1>
<p>本说明用于记录本次抽样测算的取数口径与参数设置，供后续复测时比对使用。
测算范围覆盖全部 eight 个区域销售单元，未剔除异常小额订单。</p>
<p>参数设置方面，识别阈值沿用默认值，未做逐件调优；
表格结构模型选用通用版，未启用无线表专用检测分支。</p>
<p>如需复现本表结果，应保持上述参数不变，并使用同一版本的识别模型资产。</p>
</div>
"""


def main() -> None:
    ASSETS.mkdir(parents=True, exist_ok=True)
    make_bar_chart(ASSETS / "chart_bar.png")
    make_sparkline(ASSETS / "spark.png")
    make_seal(ASSETS / "seal.png")
    html = f"<!doctype html><meta charset='utf-8'><style>{BODY_CSS}</style>{P1}{P2}{P3}{P4}"
    HTML(string=html, base_url=str(ASSETS)).write_pdf(
        PDF, stylesheets=[CSS(string=BODY_CSS)])

    gt = {
        "_comment": "ground truth 由 tests/samples/gen/make_synth_samples.py 生成过程给出",
        "page_size_pt": [595, 842],
        "pages": [
            {
                "page": 1, "backlog": "#10 chart", "kind": "chart",
                "region": "页面中部（标题与图注之间）",
                "expect_in_chart_block": [CHART_LABELS["title"], *CHART_LABELS["legend"],
                                          CHART_LABELS["xlabel"], CHART_LABELS["ylabel"],
                                          *CHART_LABELS["ticks"], *CHART_LABELS["values"]],
                "assert": "图内文字不并入正文流；图区与图注各自成块",
            },
            {
                "page": 2, "backlog": "#9 修法5", "kind": "table_with_image",
                "region": "第 2 行「工业模组」行「趋势」列单元格内",
                "expect_image_in_table": True,
                "assert": "折线图被判为 Image 且其 bbox 落在表格 bbox 内（图内无文字）",
            },
            {
                "page": 3, "backlog": "#7 ANYDOC_WIRELESS_CELLS", "kind": "tight_wireless_table",
                "region": "整页表格（8 行 6 列无线表，行高压缩至 1.0）",
                "expect_in_table": [r[0] for r in TIGHT_ROWS] + [TIGHT_ROWS[-1][0]],
                "assert": "紧行高下无线表单元格不粘连、不丢行；供 ON/OFF 复测",
            },
            {
                "page": 4, "backlog": "#5a ANYDOC_SEAL_ARC", "kind": "arc_seal",
                "region": "页面右上角（38mm 见方）",
                "expect_seal_arc_text": SEAL_ARC_TEXT,
                "expect_seal_straight_text": "档案专用",
                "assert": "环排文字与横排文字各自处理；弧排默认关闭时该块可跳过",
            },
        ],
    }
    GT.write_text(json.dumps(gt, ensure_ascii=False, indent=2), encoding="utf-8")
    print(f"[ok] {PDF}  ({PDF.stat().st_size / 1024:.0f} KB)")
    print(f"[ok] {GT}")
    for p in sorted(ASSETS.iterdir()):
        print(f"     asset: {p.name} ({p.stat().st_size / 1024:.0f} KB)")


if __name__ == "__main__":
    main()
