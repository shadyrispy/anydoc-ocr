#!/usr/bin/env python3
"""构造最小含图 docx 样本，验证 anydoc assets 消费通路。
生成一张白底黑字位图 PNG（手绘 "OCR" 字样），打包进 docx。
"""
import struct, zlib, zipfile, os

OUT = os.path.join(os.path.dirname(__file__), "with_image.docx")

def make_png(w, h, pixels):
    """pixels: bytes of RGBA, w*h*4 length."""
    def chunk(typ, data):
        c = typ + data
        return struct.pack(">I", len(data)) + c + struct.pack(">I", zlib.crc32(c) & 0xffffffff)
    sig = b"\x89PNG\r\n\x1a\n"
    ihdr = struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)
    raw = b""
    for y in range(h):
        raw += b"\x00" + pixels[y * w * 4:(y + 1) * w * 4]
    idat = zlib.compress(raw)
    return sig + chunk(b"IHDR", ihdr) + chunk(b"IDAT", idat) + chunk(b"IEND", b"")

# 240x80 白底，中间画黑色 "OCR" 位图（手绘 5x7 点阵放大）
W, H = 240, 80
px = bytearray(b"\xff" * (W * H * 4))  # 白色 RGBA

# 5x7 点阵 O C R (每字符 5 列宽，字符间距 1 列)，放大 8x，起始 x=40,y=16
# O: 01110 10001 10001 10001 10001 10001 01110
# C: 01110 10001 10000 10000 10000 10001 01110
# R: 11110 10001 10001 11110 10100 10010 10001
glyphs = {
    "O": ["01110","10001","10001","10001","10001","10001","01110"],
    "C": ["01110","10001","10000","10000","10000","10001","01110"],
    "R": ["11110","10001","10001","11110","10100","10010","10001"],
}
text = "OCR"
scale = 8
gx, gy = 40, 16
for ci, ch in enumerate(text):
    rows = glyphs[ch]
    for ry, row in enumerate(rows):
        for rx, bit in enumerate(row):
            if bit == "1":
                for dy in range(scale):
                    for dx in range(scale):
                        x = gx + ci * 6 * scale + rx * scale + dx
                        y = gy + ry * scale + dy
                        if 0 <= x < W and 0 <= y < H:
                            off = (y * W + x) * 4
                            px[off:off+3] = b"\x00\x00\x00"  # 黑色
png = make_png(W, H, bytes(px))

CT = '''<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Default Extension="png" ContentType="image/png"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>'''

RELS = '''<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>'''

DOC = '''<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
<w:body>
<w:p><w:r><w:t>Embedded image test:</w:t></w:r></w:p>
<w:p><w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="2286000" cy="762000"/><wp:docPr id="1" name="Picture 1"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:nvPicPr><pic:cNvPr id="1" name="image1.png"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed="rId1"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="2286000" cy="762000"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>
</w:body>
</w:document>'''

DOC_RELS = '''<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/>
</Relationships>'''

with zipfile.ZipFile(OUT, "w", zipfile.ZIP_DEFLATED) as z:
    z.writestr("[Content_Types].xml", CT)
    z.writestr("_rels/.rels", RELS)
    z.writestr("word/document.xml", DOC)
    z.writestr("word/_rels/document.xml.rels", DOC_RELS)
    z.writestr("word/media/image1.png", png)

print(f"wrote {OUT} ({os.path.getsize(OUT)} bytes)")
