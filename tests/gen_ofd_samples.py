#!/usr/bin/env python3
"""Generate synthetic text-type and image-type OFD files for end-to-end tests.

Builds minimal-but-valid OFD 1.0 packages by hand (namespace-free XML, which
ofd-core's serde matches on local names) and zips them. For the image-type
case a real PNG (Chinese text burned in via PIL) is embedded as a MultiMedia
resource so the render -> OCR path has actual pixels to read.
"""
import io
import os
import zipfile

from PIL import Image, ImageDraw, ImageFont

SAMPLES = os.path.join(os.path.dirname(__file__), "samples")
CJK_FONT = "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"

OFD_XML = """<?xml version="1.0" encoding="UTF-8"?>
<OFD Version="1.0" DocType="OFD">
  <DocBody>
    <DocInfo><DocID>{doc_id}</DocID></DocInfo>
    <DocRoot>Doc_0/Document.xml</DocRoot>
  </DocBody>
</OFD>
"""

DOC_XML = """<?xml version="1.0" encoding="UTF-8"?>
<Document>
  <CommonData>
    <MaxUnitID>{max_id}</MaxUnitID>
    <PageArea>
      <PhysicalBox>0 0 210 297</PhysicalBox>
    </PageArea>{res}
  </CommonData>
  <Pages>
    <Page ID="1" BaseLoc="Pages/Page_0/Content.xml"/>
  </Pages>
</Document>
"""

CONTENT_HEAD = """<?xml version="1.0" encoding="UTF-8"?>
<Page>
  <Area><PhysicalBox>0 0 210 297</PhysicalBox></Area>
  <Content>
    <Layer Type="Body">
"""

CONTENT_TAIL = """    </Layer>
  </Content>
</Page>
"""

TEXT_OBJ = """      <TextObject ID="{oid}" Boundary="{bx} {by} {bw} {bh}" Font="1" Size="6">
        <TextCode X="{x}" Y="{y}">{text}</TextCode>
      </TextObject>
"""

IMAGE_OBJ = """      <ImageObject ID="{oid}" Boundary="{bx} {by} {bw} {bh}" ResourceID="{rid}"/>
"""

RES_XML = """<?xml version="1.0" encoding="UTF-8"?>
<Res BaseLoc="Res">
  <MultiMedias>
    <MultiMedia ID="{rid}" Type="Image" Format="PNG">
      <MediaFile>{media}</MediaFile>
    </MultiMedia>
  </MultiMedias>
</Res>
"""

# Public resource declaring a font (no embedded file -> renderer falls back to
# the matching system font). Lets --ofd-force-ocr rasterize text pages.
PUBLIC_RES_XML = """<?xml version="1.0" encoding="UTF-8"?>
<Res BaseLoc="Res">
  <Fonts>
    <Font ID="1" FontName="Noto Sans CJK SC" FamilyName="Noto Sans CJK SC"/>
  </Fonts>
</Res>
"""


def write_ofd(path, entries):
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        for name, data in entries.items():
            z.writestr(name, data)


def text_ofd(doc_id, lines, with_font_res=False):
    """lines: list of (x_mm, y_mm, text). Boundary derived per line.
    with_font_res=True adds a PublicRes so --ofd-force-ocr can rasterize."""
    objs = []
    for i, (x, y, t) in enumerate(lines, start=1):
        objs.append(TEXT_OBJ.format(oid=i, bx=x - 2, by=y - 6, bw=200, bh=10, x=x, y=y, text=t))
    content = CONTENT_HEAD + "\n".join(objs) + CONTENT_TAIL
    res_tag = '\n    <PublicRes>PublicRes.xml</PublicRes>' if with_font_res else ""
    entries = {
        "OFD.xml": OFD_XML.format(doc_id=doc_id),
        "Doc_0/Document.xml": DOC_XML.format(max_id=len(lines) + 1, res=res_tag),
        "Doc_0/Pages/Page_0/Content.xml": content,
    }
    if with_font_res:
        entries["Doc_0/PublicRes.xml"] = PUBLIC_RES_XML
    return entries


def image_ofd(doc_id, png_bytes, rid=10):
    media = "Image_0.png"
    content = CONTENT_HEAD + IMAGE_OBJ.format(oid=20, bx=10, by=10, bw=190, bh=277, rid=rid) + CONTENT_TAIL
    entries = {
        "OFD.xml": OFD_XML.format(doc_id=doc_id),
        "Doc_0/Document.xml": DOC_XML.format(
            max_id=20,
            res="\n    <DocumentRes>DocumentRes.xml</DocumentRes>",
        ),
        "Doc_0/DocumentRes.xml": RES_XML.format(rid=rid, media=media),
        "Doc_0/Res/" + media: png_bytes,
        "Doc_0/Pages/Page_0/Content.xml": content,
    }
    return entries


def make_text_image(lines, size=(820, 1160), font_size=42):
    img = Image.new("RGB", size, (255, 255, 255))
    draw = ImageDraw.Draw(img)
    try:
        font = ImageFont.truetype(CJK_FONT, font_size)
    except Exception:
        font = ImageFont.load_default()
    y = 40
    for ln in lines:
        draw.text((40, y), ln, fill=(0, 0, 0), font=font)
        y += font_size + 28
    buf = io.BytesIO()
    img.save(buf, format="PNG")
    return buf.getvalue()


def main():
    os.makedirs(SAMPLES, exist_ok=True)

    # 1) Text-type OFD: Chinese text extracted directly from TextObjects.
    text_lines = [
        (20, 35, "文档标题样例"),
        (20, 65, "这是第一段中文内容，用于验证文字型 OFD 提取。"),
        (20, 90, "第二段：发票号码 2024001，金额 1280.00 元。"),
    ]
    write_ofd(os.path.join(SAMPLES, "text.ofd"), text_ofd("text-sample", text_lines))

    # 1b) Same text OFD but with a system-font resource, so --ofd-force-ocr
    #     can rasterize the page and OCR it (validates the render+CJK path).
    write_ofd(os.path.join(SAMPLES, "text_font.ofd"),
              text_ofd("text-font-sample", text_lines, with_font_res=True))

    # 2) Image-type OFD: a rasterized page (Chinese text baked into pixels).
    png = make_text_image([
        "发票样例",
        "购买方：测试科技有限公司",
        "金额：1280.00 元",
        "发票号码：2024001",
    ])
    write_ofd(os.path.join(SAMPLES, "image.ofd"), image_ofd("image-sample", png))

    # 3) Image-type OFD with a table-like layout.
    png_t = make_text_image([
        "费用报销表",
        "项目        金额",
        "交通费      200",
        "餐饮费      380",
        "合计        580",
    ])
    write_ofd(os.path.join(SAMPLES, "image_table.ofd"), image_ofd("image-table-sample", png_t, rid=11))

    print("wrote:", sorted(os.listdir(SAMPLES)))


if __name__ == "__main__":
    main()
