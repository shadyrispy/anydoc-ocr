# Backlog（已取证、未完成）

本文件记录**做过完整根因取证、但暂未实现**的项，避免下次从零重探。
每条含：现象、取证结论、已试过且失败的方案、下一步设计。

---

## #5a 印章环排（弧形）文字识别

状态：**部分完成**（#5a 本体仍停在这里，未随 #10b 推进）。直排行已通且**自 #10b 起默认开启**（`ANYDOC_NO_SEAL_OCR` 关闭）；环排公司名显式跳过。

### 已交付（本次）

- `src/seal.rs` 内核：IoU 嵌套框去重、`join_seal_lines` 行规整、
  `is_curved_band` 弧行判定（MinerU `get_poly_rect_crop` 的 cover≥0.7 口径）。
- `src/ocr_post.rs` 后处理通路：版面 `Seal` bbox → 页图裁剪 → `seal_ppocrv4_det`
  行检测 → min-area-rect quad 摆正 → tier rec → 文本写回 `LayoutElement.text`。
- `gfm_adapter` 输出 `【印章】…` 行；`ocr_engine` 的 `EngineKey` 已含 `seal_ocr` 位
  （开关切换必然重建引擎，防串会话）。
- 端到端实测（`tests/samples/seal_scan.pdf`，150dpi）：
  `【印章】专用章` 正确产出；弧行按判定跳过。
- 默认路径零成本：开关关闭时 `post: None`，不建 session，输出逐字节不变。

### 待做：弧行矫正（`unroll_arc_band`）

目标：章顶 126° 环排「北京测试科技有限公司」→ 拉直成水平条带给 rec。

**取证事实**（勿重复探索）：

1. `SealTextDetectionPredictor` 输出**沿弧走行的 90/89 点多边形**，不是 4 点框；
   `get_rotate_crop_image` 严格要求 4 点 → 早期版本因此**整行丢弃**。
2. fixture 实测 cover：章底「专用章」≈ **0.90**（直），章顶环排 ≈ **0.50**（弧）
   —— 分流阈值 0.7 两侧余量都很大，判定稳定。
3. MinerU 参考实现（本机 `/usr/local/lib/python3.12/dist-packages/mineru`）：
   - `model/ocr/seal_crop.py` `CropByPolys(det_box_type="poly")`：先算 minAreaRect
     裁剪 `temp_crop_img`；IoU(polygon, rect) ≥ 0.7 → 直接用它；否则重采样上下边线
     + `AutoRectifier` **单应变换**。
   - `SortPolyBoxes` = 按 y_min 排序（我们已同口径）。
   - `pp_ocr_v6_onnx.py` seal 模式参数：`limit_side_len=736`、`thresh=0.2`、
     `box_thresh=0.6`、`unclip_ratio=0.5`、`box_type="poly"`、`drop_score=0.0`、
     **不做 det 框合并**。
   - 结论：MinerU 靠**单应 rectifier**，不是极坐标展开。

**已试过且失败的两条路**（都有单测复现，别再回退到它们）：

- (a) **凸包 + Kåhr 最小二乘圆拟合章心**：浅弧病态。126° 弧圆心偏 **5.95px**
  （容差 1.0）；±π 分支弧段的角跨度解缠算成 **3.06 rad**（真值 ≈1.2）。
  根因：凸包含内弧角点偏置拟合，且极角空档切割点错位。
- (b) **边界链按弧长配对**（切两条链 → 等弧长重采样 → 逐列连线）：
  弧带的两端**径向边**必须归属某条链，配对相位因此错位；数值验证同一 fixture
  形态下 `arc_len = 285.2`（真值 270.6，+5.4%）、`thmin ≈ 0.1`（列内出现零宽度），
  且 ∪ 弧的字头/字脚判反。要修就得先检测并剔除端帽边，复杂度失控。

**下一步设计（推荐）**：**极坐标展开 + 外部供给章心**。章心**不需要拟合**——
版面给的 Seal bbox 是环形章的外接框，`crop_clamped` 裁出的裁剪图**几何中心即章心**
（实测 crop 395×389，弧轴 (66,18,364,155) 相对中心对称）。于是：

- 圆心 = `(crop.width()/2, crop.height()/2)`（若 bbox 非正方形，按短边折算半径）；
- 每列输出 `x ↔ θ` 线性映射到多边形在该 θ 上的径向极值区间 `[r_lo(θ), r_hi(θ)]`
  （由多边形顶点按 θ 分桶取 min/max，无需链配对）；
- 字头方向：章顶弧（θ 均值 < 0，屏幕 y 向下）字头朝**外缘**，章底弧相反；
- 分流仍在 `is_curved_band`：判直 → quad（已实现），判弧 → 走展开（待实现）；
- 验收判据（务必用**内容摆放**而非几何量回归）：在合成弧带的已知角度/半径处画墨块，
  断言矫正后落在预期的 (列, 行) 半区——本轮就是靠这条抓到 ∪ 弧字头反了的问题。

风险：rec 模型（`PP-OCRv5_rec` 系）对章内红字 + 低对比弧排文本的召回本身有限，
展开几何对了也可能识别不全；先在 fixture 上量一轮再决定是否需要印章色通道分离
（R 通道减 B 通道可显著提对比，成本一个 pixelwise pass）。

---

# MinerU 4.0.5 缺口清单（#6–#14）

**口径来源**：本机安装的 MinerU **4.0.5** 全量能力面盘点见
`.claude/artifacts/mineru-4.0.5-surface.md`（593 行，每条结论带 `file:line` 证据；
本仓 `/.claude/` 在 `.gitignore:24` 内，该文件只存于工作区，不在仓库里——
与 `designs/anydoc-ocr-arch-refactor.md` 同一约定）。
本节条目是那份清单与本仓逐项对照后的缺口，编号接续 #5a/#1。

**先记一条定性事实**（决定这些条目的优先级）：MinerU 的默认档是
`tier=None → "standard"`（`mineru/parser/tier.py:53`），standard = 小模型 + **VLM**
（无 `server_url` 时要装 VLM 引擎模块，`tier.py:71-77`）；`basic` 才是
"layout + MFR + 表格模型、无 VLM"（映射 effort=medium）。本仓的 `mineru-basic`
对齐的是 MinerU 的**第二低档**。这不是缺陷，但 README/`--help` 必须写清对齐口径，
否则用户拿本仓输出与 `mineru` 默认跑的结果对比会误判成 bug（见 #14）。

### 索引：问题 ↔ 执行 ↔ 完成判定

按"用户看得见的那一面"重排一遍，每行都能独立读懂；细节见对应小节。

| # | 问题（谁会被卡住） | 第一个动作（第 0 步，不许跳过） | 完成判定 | 依赖 · 规模 |
|---|---|---|---|---|
| #6 | 只有 markdown 一种输出；想加任何结构化输出都得改主链路 | 用 `ANYDOC_DUMP_DIR` 对照 MinerU item 字段，**列** IR 缺失字段清单（块边界/标题级别/span/页尺寸），不改代码 | 块级类型+级别+bbox+span 可单测断言，且 markdown 对 6 样本 hash 全等（**未**用 UPDATE） | **第 0/1/2/3/4 步已完成，第 5 步枚举先行已落地**（`RegionKind` 扩 Image/Code/Formula/Index/Aside/Footnote/Noise，前五类占位、`Footnote`/`Noise` 有 producer，**输出逐字节不变、未用 UPDATE**）、**#10 例外项已落地**（页眉页脚"标注 + 可选输出"，`ANYDOC_EMIT_FURNITURE`）、**决策 (c) 已落地**（`ANYDOC_RICH_TEXT` 废弃：行为移除 + stderr 告警 + `--help`/README 标废弃）；第 6 步起 · 大 |
| #7 | 无线表/合并单元格表的结构识别**怀疑**不如 MinerU basic | 拿同一张无线表跑「现状」vs「`rt-detr-l_wireless_table_cell_det.onnx` cells→HTML（已在注册表）」，比结构正确率；并确认 UNet 能否进 `with_wireless_table_structure` | 无线表与 MinerU basic 的表格 HTML 结构不一致数**下降**；有线表逐字节不变 | **第 0 步已完成、两条路都不进默认**（(ii) 判死，(i) 已实现为默认关闭的 `ANYDOC_WIRELESS_CELLS`）· 剩余部分待现网语料 |
| #10b | 含章公文页默认不出印章文字，而 MinerU basic 默认出 | 决定"默认开"能否接受为行为变更（普通安装多 4.8MB 下载 + 新增 `【印章】` 行） | seal_scan 默认档出直排行文字 + golden 重基线 + README 记一笔 | **已完成** · 小 |
| #12 | `anydoc scan.png` 进不了 OCR 管线（MinerU 直接吃 8 种图片） | `detect` 加 `DocKind::Image`，复用整页 OCR；像素闸要在**加载**处补算一次 | png 出 markdown；>3500px 显式 `resourceLimit`；gif/tiff 只取首帧且 `--help` 注明 | **已完成** · 小（本节最便宜） |
| #13 | 无法强制"只用文字层、绝不联网下载模型" | 定名：`--pdf-text-only` 还是 `--ocr-mode txt`（**只留一套**） | 空 `$OAR_HOME` + 断网实测不加载模型 | **已完成**（定名 `--text-only`）· 小 |
| #8 | 图片型扫描件里旋转的表格没有第二道方向信号（表被转置/正文被吞） | 先查 `with_table_orientation`（`structure.rs:442`）要吃哪个模型、是否在我们注册表 | 合成旋转表在 `--pdf-force-ocr` 下网格正确；非旋转页逐字节不变 | **已实现为默认关闭的 `ANYDOC_TABLE_ORI`**（90/180/270 三角度 ON 严格更好、6 件既有表逐字节不变）· 翻默认待现网语料误判率 |
| #9 | 表格里的公式/图片丢；行内公式是否已对齐**未知** | 拿含行内公式与公式编号的样本 `ANYDOC_DUMP_DIR` 对拍，先量"已具备/缺失"再决定做多少 | 行内公式 `$...$` 不掉行；编号不重复成独立行；表内对象有去处 | **第 0 步已完成**：缺口是真的但**根因在装配层**（上游 stitching 好的 `LayoutElement.text` 我们没用），不需接新模型；表内图那半**仍未量到** · 中 |
| #10 | code 无 fence、目录无缩进、旁注混进正文、脚注/引用不挂接 | 按 `PIPELINE_DET_TYPE` 13 项内顺序 CODE → INDEX → ASIDE_TEXT → FOOTNOTE/REF_TEXT，各配 1 个样本 | 每类输出结构可断言；未涉及类型逐字节不变 | 除页眉页脚外全依赖 #6 · 中-大 |
| #11 | 没有可被下游消费的产物（content_list / middle_json） | #6 完成后，先把 24 个类型名与 bbox 约定抄成**单测常量表**，再写 renderer | 与 MinerU basic 同文档的类型序列/块数对齐率可量化；markdown 输出不变 | #6 · 中 |
| #14 | 与 MinerU 默认档（standard，含 VLM）精度不可比 | 只写文档口径；如要接，只做 `--server-url` 客户端，不搬权重 | README/`--help` 口径落地即结 | **已完成**（README + `--help` 口径已落地）· 文档级 |

**开工顺序建议**：#12 / #13 / #10b（小而独立，先攒收益）→ **#7**（当前最大的实际质量偏差）
→ **#6**（结构债的根，之后 #11/#10 才有落点）→ #8 / #9 / #10 / #11。

---

## #6 结构表示：IR 为真相，MinerU schema 作投影（阻塞 #11）

状态：**决策已定，未实施**。这是本节唯一需要先做架构决断的条目。

**问题 → 动作**：输出面只有 markdown，加任何结构化输出都要动主链路 → 先做**字段清单**（第 0 步），不要先动 `region.rs`。

### 决策与理由

不新增第二套真相，也不把 DocIR 换成 middle_json 形状：**DocIR 扩成真正的结构 IR，
`middle_json` / `content_list_v2` 作为它的 renderer 输出**。

1. `content_list_v2` 在 MinerU 里是**派生物**：`render_content_list_v2(middle_json, ...)`
   （`mineru/render/_internal/content_list/v2.py:58-70`）。绕不过结构层，所以"只出
   content_list"这条路不成立，必须先有结构表示。
2. **不要投资 legacy 格式**：`backend/postprocess/legacy_middle_json.py:1-4` 自述
   "两个历史分支集中在此，**未来可一并移除**"；页面字段是
   `{page_size, preproc_blocks, para_blocks, discarded_blocks}`
   （`legacy_middle_json.py:16`）。对着一个官方计划删除的形状做兼容层，是负资产。
3. MinerU 4.0.5 的**严格** MiddleJson/Block 类层次来自外部框架
   `docvortex.schema`（`types.py:6-85` 的 import），不在 MinerU 包内 → 逐字段复刻其
   内部对象模型缺乏权威 schema，只能用 dump 样本反推。故对齐点定在
   **输出 schema 的字段语义**（类型名、bbox 约定、span 结构），而非内部表示。
4. 本仓已有先例支撑"IR 加字段、渲染层分流"的做法：P1.5 就是三源统一产 DocIR、
   渲染层只消费 IR（`src/docir/mod.rs:1-20`），新增 `kind`/`confidence` 两字段即
   完成（`src/region.rs:26-49`）。#6 是同一模式的延续，不是重写。

### 落地前提：现在的 DocIR 是文本流 IR，不是结构 IR

取证事实（这是本条目的真正工作量所在）：

- ~~`RegionKind::Body` 的 `text` 已由 producer 完成"阅读顺序还原 + **标题前缀注入**"~~
  —— **第 2 步已消除**：`text` 现在是未加前缀的行文本，级别在 `Region.heading_level`。
  这条取证事实保留，是为了说明"字符串焊死"这个起点确实存在过（原注释见
  `git show 8b46773:src/region.rs`）。
- `RegionKind::PreRendered` 存的是"成品 markdown 片段（含精确分隔符），渲染层原样
  追加、不二次加工"（`src/region.rs:31-34`）；
- 块级语义只剩 4 个 kind（Body/Grid/TableHtml/PreRendered），**没有**：标题级别、
  header/footer/page_number 的区分（现被当噪声丢弃，见 `src/gfm_adapter.rs:5-6`）、
  span（行内粗斜体/行内公式）、图片资产引用、页尺寸（只在 `ANYDOC_DUMP_DIR` 的旁路
  JSON 里，`src/pdf/mod.rs:299-301`）。

→ 从当前 IR **无法**还原出块边界与级别，字符串已经焊死了。因此 #6 的第一件事是
把结构信息从 producer 的输出字符串里**解耦**：producer 产"块 + 级别 + span"，
`#` 前缀与分隔符下移到渲染器。

### 第 0 步产出：字段清单（2026-09-27，逐条实读权威 schema）

**权威来路更正**：严格 schema 不在 MinerU 包内，而在 **`docvortex/schema.py`**
（MinerU `types.py` 从那里 re-export），所以下面每条的证据都是这个文件。对照
MinerU 侧渲染层 `render/_internal/content_list/v2.py`（其 `:1-4` 自述"严格
MiddleJson 到按页 Content List V2 的渲染实现"，即 content_list 确为派生物）。

**先纠正一处会被反复误用的口径**：严格 middle_json 的 `bbox` 是 **0–1 归一化**，
不是 pt——`BlockBase.bbox` 校验器直接要求每个分量 `0.0 ≤ v ≤ 1.0`
（`schema.py:463-485`，`schema.py:481` 那行 raise "must be finite normalized
coordinates"）。content_list v2 再乘 1000 成整数框
（`content_list/common.py:118-122` `normalize_bbox`，docstring 明写"把 MiddleJson
的 0-1 bbox 转换为 Content List 的 0-1000 整数框"）。
→ 我们侧是 **pt、左上原点、y 向下**（`src/region.rs:1-6`）。所以投影**必须先有页尺寸
才能算归一化**，而 `PageIR` 现在只有 `page_no`/`regions`/`source` 三个字段
（`src/docir/mod.rs:36-44`），页尺寸只活在 dump 的 `doc*_dims.txt` 旁路里
（`src/pdf/mod.rs:357-359`）。这条是 #6 第 1 步的真实动因，不是"顺手加个字段"。

**子代理那份 29 行差距表不可信，已逐条推翻的 5 处**（别再引用它）：

1. "MinerU page 有 `page_w`/`page_h`/`rotate`" —— 严格 `PageInfo` **只有
   `page_idx` + `blocks`**（`schema.py:1035-1039`），没有任何尺寸/旋转字段；
   `page_size` 只存在于 **legacy** 形状（`legacy_middle_json.py:16`
   `_LEGACY_PAGE_FIELDS = {page_size, preproc_blocks, para_blocks, discarded_blocks}`），
   而本节决策第 2 条已明确"不投资 legacy"。
2. "`span.font` / `span.size`" —— `TextSpan` 只有 `type`/`content`/`styles`
   （`schema.py:341-346`）；全文件 grep `font`/`size`/`font_size` **零命中**。
   字体与字号**不在** MinerU 严格 schema 面，不是我们的缺口。
3. "`table.cells[].row`/`.col`、`row_span`/`col_span`" —— 不存在这些类。表格 body
   是 `TableBodyBlock`（`schema.py:598`）继承 `ImagePayloadContentBlock`，载荷就是
   **一个 `content: str`（HTML）**（`:584-588`）。跨格信息在 HTML 字符串里，不在字段里。
4. "标题级别已丢失"分类正确，但 MinernU 侧形状被写成"doc_title/paragraph_title +
   级别"这种模糊说法 —— 实际是显式 `level: int` 字段：`TitleBlockBase.anchor` +
   `level`（`schema.py:521-525`），`DocTitleBlock.level` 恒 **1**（`ge=1, le=1`），
   `ParagraphTitleBlock.level` **2..6**（`ge=2, le=6`）。→ 我们的 `#` 前缀级数
   与之**同一数域**，映射是 1:1，不需要换算表。
5. 所有 MinerU 侧 `file:line` 当时被自述为**占位**（子代理自己标注"未验证、
   未读 MinerU 源码"），上表已用实读结果替换。

**真实差距表**（只列已双侧核过的；"分类"= 已有等价物 / 可推出 / 须改 producer）

| MinerU 严格 schema 字段 | 证据 | 我们侧 | 落点 |
|---|---|---|---|
| `bbox`（0–1 归一化，`[x0,y0,x1,y1]`） | `schema.py:463-485` | pt 左上原点 y 向下 → **可推出，但缺页尺寸** | `PageIR` 加 `page_w/page_h`（pt）+ 渲染层归一化 |
| `BlockBase.index`（页内递增序号） | `schema.py:467`；`PageInfo` 校验顶层 index 严格递增 `schema.py:1047`/`:1052` | **已有等价物**：`Vec<Region>` 的位置即阅读序 | 渲染层用下标，不新增字段 |
| `type: BlockType`（29 值，实数） | `schema.py:70-108` | **部分**：`RegionKind` 4 值（`region.rs:24-34`），无 image/code/formula/index/aside/footnote/header/footer/page_number | `region.rs` 扩枚举（#10 同源） |
| `DocTitleBlock`/`ParagraphTitleBlock.level` | `schema.py:528-536` | **已永久丢失**：级别焊在 text 前缀（`src/text_health.rs:117`/`:123` `format!("{} {}", "#".repeat(lv), line)`） | `Region` 加 `heading_level: Option<u8>`；前缀下移渲染器（**触碰字节契约**） |
| `InlineContentBlock.content: list[InlineSpan]` | `schema.py:494-503` | **已永久丢失**：无 span 概念 | 新增 `Span` + `Region.spans`（三个 producer 都要改） |
| `TextSpan.styles: [bold\|italic\|underline\|emphasis\|strikethrough\|superscript\|subscript]` | `schema.py:320-338`、`:341-356`（上下标互斥校验） | **决策 (c) 已落地**：`ANYDOC_RICH_TEXT` 废弃，`**`/`<u>` 字面量通路整体删除 → 样式现状是**完全没有**（比"非结构化"更干净） | 第 4 步直接从 pdf-inspector 的 `is_bold`/`is_italic`/几何装饰证据产 `Span`，不从 markdown 反解 |
| `EquationInlineSpan`（`equation_inline`，**不含外层定界符**） | `schema.py:358-371` | 无行内公式载体 | `SpanKind::Equation`；与 #9 对拍后再定 |
| `CodeInlineSpan` / `HyperlinkSpan` | `schema.py:373-397` | 无 | 同上（span 层一次性补齐） |
| `continues_prev: bool\|None`（仅顶层块） | `schema.py:506-509`；嵌套块禁止携带 `schema.py:1058` | **可推出**：跨页表合并 pass 已知续接关系（`docir/passes/cross_page_table`），但未存字段 | `Region` 加 `continues_prev`，pass 顺带写 |
| `TableBlock.cell_merge: list[0\|1]\|None` | `schema.py:704-709`（由 `postprocess/visual.py:440-441,476-477` 从 body 弹出后挂到父块） | **须改 producer**：`TableCell{text,x,y,h}` 无跨格信息（`src/table_grid.rs:16-21`） | 若要投影须保留 oar-ocr 的 span 信息，别从 HTML 反解 |
| `ImagePayloadBlock.image_base64\|image_path\|image_url` | `schema.py:556-581`（path 仅安全 POSIX 相对路径，URL 禁危险协议） | **已永久丢失**：IR 无图片资产引用，Image 块 bbox 只用于表格补救（`gfm_adapter.rs`） | 需新增"裁图落盘 + 引用"通路（成本最高的一项） |
| `EquationBlock`（行间公式，`content: str` + 载荷） | `schema.py:590-592` | **已永久丢失** | 同 #9 |
| `CodeBlock`/`AlgorithmBodyBlock`/`ListBlock`/`IndexBlock` | `schema.py:731-735`（`CodeBlock.sub_type` 区分 code/algorithm）、`:610-614`、`:646-662`。**两处口径别混**：`BlockType` 里 CODE/REF_TEXT/HEADER 等注释写 "Added in vlm 2.5"（`schema.py:86`、`:99-104`）说的是**版面标签来源**，不代表不在 basic 线上——`PIPELINE_DET_TYPE`（basic=medium 的检测集，`ocr.py:50-51`）**含** CODE / ASIDE_TEXT / INDEX / REF_TEXT / HEADER / FOOTER / PAGE_NUMBER / PAGE_FOOTNOTE，**不含** LIST 与 CHART（`constants.py:98-113`） | 与 #10 的 13 项口径同源（basic 不含 LIST/CHART） | 排在 #10 之后 |

**dump 实测的两条硬事实**（决定第 1 步从哪来）：

1. **文字层通路根本没有 dump**。写入点只有 `assemble_doc_result` 内那一处
   （`src/pdf/mod.rs:350-361`），而它只被 OCR 批量路径调用（`src/pdf/mod.rs:311`）；
   文字层走 `finalize_text_docir`，不经此函数 → 设了 `ANYDOC_DUMP_DIR` 也不落盘。
   子代理用"没设 env 跑 text.pdf"来证明这一点是**操作失误**（它自己也承认了），
   真正的证据是上面的调用链。**含义**：想用 dump 对拍 IR 字段，text.pdf/text.ofd
   这类原生文字层文档拿不到任何中间数据 → #6 第 1 步若要看页尺寸现状，得先补
   文字层侧的 dims 传递，或改用 OCR 样本对拍。
2. OCR dump 的形状是 `doc{i}_page{p:03}.json`（`StructureResult` 原样 serde）
   + `doc{i}_dims.txt`（`"{page} {w} {h}"` 行）（`src/pdf/mod.rs:353-359`）。
   → **页尺寸已经在手边**，只是没进 IR。这是最便宜的第 1 步。

**最小增量顺序**（每步注明是否触碰字节契约；1→2 是本票唯一必须先做的两件）

1. `PageIR` 加 `page_w`/`page_h`（pt），三个 producer 各传一次 → **不触碰**（渲染层
   不消费即输出不变）。解锁 bbox 归一化与 content_list 的 `bbox` 字段。
   **已落地，但"pt"这个前提是错的**——见下"第 1 步已落地"。
2. `Region` 加 `heading_level: Option<u8>`，producer 改"设字段"而前缀**下移到
   `docir/render.rs`** → **已落地**（见下"第 2 步已落地"）。解锁 #11 的标题级别与
   #10 的 DOC_TITLE。
   计划里标的"**触碰**字节契约"这一格**没有兑现**：原以为前缀下移必然改 markdown
   字面输出（所以决策 (a) 预备了 `ANYDOC_GOLDEN_UPDATE` 重基线），做完后 21 个 CLI
   样本与 10 条 golden 快照逐字节不变——原因见该节"为什么本步没用到 UPDATE"。
3. `Region` 加 `continues_prev`，由既有跨页表 pass 写入 → **不触碰**。**已落地**，
   但这格计划里"pass 顺带写"四个字把真正的难点写没了：pass 原先是**删掉**被吸收的
   续页区块，删了就没有任何对象可以承载 `continues_prev`。落地形状改成"续页区块
   **原位保留 + 打标记**，渲染层按标记跳过"（等价性由单测
   `absorbed_stub_renders_identically_to_deletion` 钉住，见下"第 3 步已落地"）。
4. 新增 `Span` + `Region.spans`（先只装 text + styles） → **不触碰**（默认渲染
   忽略 spans 即输出不变）。样式那一半**没有旧通路可改**：`ANYDOC_RICH_TEXT` 已随
   决策 (c) 废弃并删除，本步是从 pdf-inspector 的样式证据直接产 spans 的**第一版**。
   **已落地**（三个 producer 全接线，输出逐字节不变，见下"第 4 步已落地"）。
5. `RegionKind` 扩 Image/Code/Formula/Index/Aside/Footnote + 图片裁切落盘通路 →
   **不触碰**既有块，新类别出现才改输出（依赖 #10 的样本）。
   **枚举先行已落地**（含 `Noise(NoiseKind)`，`Footnote`/`Noise` 已有 producer，
   前五类占位等样本；图片裁切落盘留 #10 主体，见下"第 5 步已落地"）。
6. `TableGrid`/表格 HTML 侧补 `cell_merge` 与真实 row/col → **不触碰**（但若从
   HTML 反解 span 是错路，必须回到 oar-ocr 的 cell 信息，成本高）。

### 第 1 步已落地（2026-09-27）：页尺寸进 IR

**清单里"pt"这一格不成立**（三个 producer 的页尺寸根本不同单位，且 PDF 文字层
拿不到页面框）。落地形状改成"值 + 来源 + 单位"三元组（`src/docir/mod.rs`）：

- `PageDims{w,h,kind,unit}`；`kind ∈ {Unknown, PageBox, ContentExtent}`、
  `unit ∈ {Unknown, Px, Pt, Mm}`；`normalizable()` 只在 `PageBox` 且两维 >0 时为真。
- 每页记的是**该页自己区块所在坐标空间**的分母，不做任何单位换算——ADR-0008
  直提分支下位图是内嵌 image object 的原生像素（`src/pdf/render.rs:192-201`），
  与页面物理尺寸不成固定比例，反算必错。
- 三源实际取值（逐条查证，非推断）：
  | 通路 | 拿得到什么 | kind/unit | 可归一化 |
  |---|---|---|---|
  | OCR（PDF/OFD 皆然） | 送推理的位图宽高 | `PageBox`/px | ✅ |
  | OFD 文字层 | `PageObject.area.physical_box`（页 → 文档默认 → 无） | `PageBox`/mm | ✅ |
  | PDF 文字层 | **只有内容外扩**（max x+w / max y+h） | `ContentExtent`/pt | ❌ |
  | 裸图片输入 | 位图本身 | `PageBox`/px | ✅ |
- PDF 文字层为什么只有 `ContentExtent`：pdf-inspector 的 `CropBox ∩ MediaBox`
  是 `pub(crate)`（`extractor/mod.rs:14,125`），不 vendor 不改上游就拿不到真页框。
  **故意不伪造**——用内容外扩冒充分母，归一化 bbox 会系统性偏大甚至 >1。

**接线的三处非显然决定**：

1. `src/pipeline.rs` 的页尺寸采集**去掉 `ANYDOC_DUMP_DIR` 门控**。 dims 是 IR 一等
   字段，只在调试开关下填的话，正常路径会静默拿到 `Unknown`（第 0 步取证的那条
   旁路 JSON 就是唯一现状来源）。成本：每页两次 u32 读 + 一次 map 插入。
2. `assemble_doc_result` 里"map → 与 pages 同序的向量"这步抽成纯函数
   `align_page_dims(doc_idx, &[page_idx], &map)` + 两条单测。理由：`to_docir` 按
   **下标**取 dims，而 markdown 里看不见 dims，**golden 永远抓不到"页 A 配了页
   B 的分母"这类静默错配**——这是本步唯一可能悄悄错的缝，必须有非 golden 的钉。
3. `src/pdf/text_layer.rs` 的末页/可疑表探针把 `dims` 传**空**（该页是文字层页，
   成品块 `PreRendered` 的 dims 归页级 `ContentExtent`；把探针位图 px 塞进去就是
   同页两单位混用）。

**零回归证据**（本步渲染层不消费 dims，故要求"逐字节不变"，**未**用 UPDATE）：

- `cargo test --release` 全套 R=0（含 `ANYDOC_GOLDEN_OCR=1`）；
  golden 判据 **"10 checked"**、29 个快照文件校验和改动前后一致（未被写）。
- 机制性单测 `dims_do_not_affect_rendered_markdown`：传真实尺寸与传空数组的
  markdown 必须全等——比"跑一次没红"更强，因为它钉的是"渲染层不读它"这件事。
- 逐样本**文本**对拍（快照只有 16 位 hash、看不到内容，所以另建语料）：
  改前/改后各跑 21 个可产出样本的 markdown，`cmp` 全等；两个必然失败样本
  （corrupt/encrypted）的 stderr 也逐字节一致。
- 真实通路的量级证据（临时探针跑完即删）：`text.ofd` 文字层页 = 210×297
  `PageBox`/mm 且 `normalizable()`；`image.ofd` OCR 页 = 820×1160 `PageBox`/px。
  单位串了（比如 mm 写成 px）会当场露。
- **`tests/page_dims_ir.rs` 没有建**：`docir`/`gfm_adapter` 是 `pub(crate)`，
  集成测试看不见 IR，"dims 不进输出"这类断言只能在库内钉。原计划那句改指
  `gfm_adapter` / `pdf::text_layer` / `ofd` 三处同名单测。

**已知边界（别在第 2 步之后忘了）**：

- PDF 文字层页 `normalizable() == false`。#10/#11 的 bbox 投影对这类页只能出
  **未归一化 pt**，或先解决页框来源（vendor pdf-inspector / 自己用 lopdf 读
  MediaBox/CropBox），**不能**拿外扩除。
- 沙箱没有 13 个 gitignored real_samples：本步"零回归"只对 10 个可跑样本 +
  21 个 CLI 样本成立，现网语料的字面等值要到有样本的环境补跑（第 2 步 UPDATE
  时同一条限制，届时逐条 diff 只能覆盖这 10 条）。

**待决断 → 已决断（2026-09-27，用户答复，按此执行）**：

- (a) **重基线一并做**：第 2 步"前缀下移"**一次切干净**，用 `ANYDOC_GOLDEN_UPDATE`
  显式重基线，不走双写过渡。→ 本节"验收判据"里那句"**未使用** UPDATE"已作废
  （它和第 2 步天然冲突：前缀下移必然改 markdown 字面输出）。仍保留的硬要求是
  **逐条 diff 每条快照**：变化必须只落在"标题前后空行/前缀"这一类，出现任何正文
  字符差异即视为回归、当 bug 查，不许顺手接受。
- (b) **图片资产要做，但不经命令行输出面**：第 5 步的裁图落盘只做**库内通路**
  （写进输出目录/由调用方给的路径），markdown 里给**相对引用**；CLI 不新增
  "输出图片"这类参数，默认档仍是单文件 markdown。→ 分发口径不变。
- (c) **`ANYDOC_RICH_TEXT` 废弃**：统一到结构化 span 后不留渲染开关。
  废弃方式（避免"悄悄改变默认输出"）：env 不再改变行为，命中时 stderr 打一行
  废弃说明；`--help`/README 标注 deprecated 与替代（span 层）。

### 决策 (c) 已落地（2026-09-28）：`ANYDOC_RICH_TEXT` 废弃

**删掉的东西**（不是隐藏，是整体移除）：

- `src/pdf/text_layer.rs`：`rich_text_enabled*` 与 `rich: bool` 参数链
  （`build_text_docir` → `build_oriented_page` → `oriented_group_regions` →
  `build_body_regions` → `push_line_region`），`push_line_region` 恒走 `line.text()`，
  不再调 `text_with_formatting(true, true, true)`。
- `src/text_health.rs`：`apply_title_prefixes_styled` 的 `styled` 参数与
  `strip_inline_style_markers`。**判据**：这两个符号的唯一作用是"为 `ANYDOC_RICH_TEXT`
  产出的 `**`/`<u>` 字面量提供判定视图"；producer 已经不产字面量，保留一个
  永远为 `false` 的开关和它服务的正则 = 留死代码。实现留在 git 历史
  （`git log -p -- src/text_health.rs`），第 4 步做 spans 时按 spans 重做，
  不回头复活正则路线。
- `tests/pages_rich_text.rs` 旧用例 `rich_text_off_by_default_on_when_enabled`、
  单测 `rich_text_switch`。

**新增的东西**：

- `src/convert.rs::warn_deprecated_env`：存在即命中（不限值，与原判据一致）、
  `OnceLock` 每进程一次的 stderr 告警。放在 `route_doc` 开头而不是文字层分支里，
  理由是**与文档类型无关**——挂在 PDF 文字层分支会让"OFD / 纯扫描件 + 该变量"
  静默无提示，而 `route_doc` 是单文档 / `BatchConverter` / 库入口的必经汇合点。
- 契约测试两条：`rich_text_env_is_a_no_op_with_notice`（设与不设 stdout 逐字节
  相同 + 不出 `**` + stderr 有告警 + `=0` 同样告警 + `--help` 写明废弃）、
  `rich_text_notice_prints_once_per_process`（两文件批处理只告警一次）。
- `MINERU_ENGINE_HELP` 增"已废弃变量"一节（此前该变量**从未**出现在 `--help`，
  所以这是新增文档面而非改文档面）；README 环境变量表与"限制"一节同步改口径。

**样本 `rich_text.pdf` 保留**：它是仓内唯一带 bold/italic 字体证据的文字层 PDF，
废弃后用于钉**反面**契约（纯文本里不得冒出样式字面量）。`gen_rich_text.py` 的
docstring 已改写用途，避免下一个人以为它服务于一个还存在的行为。

**零回归证据**（本步不涉及任何默认行为变化，故同样要求逐字节不变，**未**用 UPDATE）：

- `cargo test --release` 全套 R=0；golden（`ANYDOC_GOLDEN_OCR=1`）
  **"OK: 10 checked, 13 skipped"**，29 个快照文件的 `md5sum | md5sum` 前后一致
  （`1d266f9281c7c6d79f5c3093ef620bc5`）且 `git status` 对快照目录零改动。
- 逐样本文本对拍：21 个可产出样本的 markdown 与第 1 步的 after 语料 `cmp` 全等；
  21 个 `.err` 同样逐字节一致（告警只在设了废弃变量时出现，默认路径 stderr 不变）。
- 单测数：lib 239 → 238。删 3（`rich_text_switch`、
  `strip_inline_style_markers_pairs_only`、`styled_title_judgment_injects_on_original_line`），
  加 2（`deprecated_env_presence_matches_legacy_switch_semantics`、
  `literal_style_markers_are_no_special_case`）→ 净 −1。

### 第 2 步已落地（2026-09-28）：标题级别进 IR，`#` 前缀下移到渲染层

**改了什么**（一次切干净，无双写、无过渡开关，按决策 (a)）：

- `src/region.rs`：`Region` 加 `heading_level: Option<u8>` +
  `with_heading_level()` / `leading_hash_level()` / `rendered_line()` /
  `is_heading()` / `is_heading_trimmed()`；新增 `HEADING_LEVEL_MAX = 6`。
  **级别是数据，`#` 是渲染产物**——`Region.text` 不再含前缀。
- `src/text_health.rs`：`apply_title_prefixes` → `title_levels(...) -> Vec<Option<u8>>`
  （判定逻辑一字未改，只是不再拼字符串）；新增 `body_regions(lines, levels)` 供
  三通路共用尾步。
- 三处 producer 改为**赋级别**：`pdf/text_layer.rs::build_body_regions`、
  `ofd/mod.rs` 普通页分支、`gfm_adapter.rs::to_docir`（原 `apply_title_prefixes`
  包装函数删除，`title_hints` 保留原样）。
- `src/docir/render.rs`：正文循环改收 `&Region`，`is_heading` 用
  `Region::is_heading()`，行文本用 `Region::rendered_line()`；OFD 分支的
  `join("\n")` 改成等价循环（原来 join 的是 `&str`，现在要逐项渲染）。
- `src/gfm_adapter.rs`：`merge_isolated_markers` / `is_noise_fragment` 收发 `Region`
  ——它们原先靠"producer 已写的 `#` 字面量"判标题，现改判**渲染视图**，
  口径不变。

**两个刻意保留的旧语义**（改动时最容易顺手"修好"的地方，都不是 bug）：

1. 来源文本自带 `#` 字面量（markdown 被印进 PDF/OFD 文字层、OCR 读到 `#` 行）：
   级别由字面量的 `#` 段数给出，`rendered_line` **不再叠加前缀**——这是旧
   "防双重标记"规则的原样搬迁。`is_heading()` 用不 trim 的渲染视图，与旧
   `t.starts_with('#')` 同口径，所以"`  # 字面量`"这种形态两版都判为非标题行、
   不加空行。两条都由单测钉住（`existing_hash_prefix_is_not_doubled`、
   `levels_then_render_equals_legacy_prefixes`）。
2. `reading_order::merge_into_paragraphs` 里的 `starts_with('#')` **不动**：它在
   装配阶段跑，那时级别还没赋（改造前同样没改），它唯一能看见的 `#` 就是来源
   字面量。注释已按新事实重写，避免下一个人以为"漏接了 IR 级别"。

**为什么本步没用到 UPDATE**：前缀下移改的是"字面量由谁写出"，不是"写出什么"。
`title_levels` + `rendered_line` 与旧 `apply_title_prefixes` 在每一行上都产出同一
字符串，`render()` 的空行判据又与旧 `t.starts_with('#')` 同口径 → markdown 逐字节
不变。故 `ANYDOC_GOLDEN_UPDATE` 这条**授权用上了但没用**，第 3 步起若真改字面输出
再启用。

**零回归证据**：

- `cargo test --release` 全套 R=0；golden（`ANYDOC_GOLDEN_OCR=1`）
  **"OK: 10 checked, 13 skipped"**，29 个快照 `md5sum | md5sum` 仍
  `1d266f9281c7c6d79f5c3093ef620bc5`，`git status` 对快照目录零改动。
- 逐样本**文本**对拍：**最终提交态二进制**（`c334106d…`，脚本 `/tmp/cap_step2_final.sh`）
  的语料 `/tmp/md_step2f` 与决策 (c) 语料 `/tmp/md_dep` 逐件 `cmp`，**44/44 全等**
  = 21 个 `.md` + 23 个 `.err`（两个必然失败样本 corrupt/encrypted 只有 stderr）。
  stderr 一起比是防"输出没变但告警/诊断变了"这种 markdown 检不出的回归。
  （先跑的 `49cefa23…` 那轮同样 44/44，但它晚于两处注释改动的重新编译，故以
  `c334106d…` 这轮为准——这条自证流程本身别省：二进制 md5 不同就是不同产物。）
- 反面证据（防"标题根本没渲染出来"这种假绿）：语料里仍有 5 行 `## ` 开头的标题行
  （`## OCR Test 123` ×3、`## Monthly Report`、`## 1. General Rules`），
  即 `rendered_line` 确实在写前缀。
- `ANYDOC_HEADINGS_LAYOUT=1` 单独对拍 4 个样本（OCR `image.pdf`/`mixed_scan.pdf`、
  文字层 `rich_text.pdf`、OFD `text.ofd`）**最终二进制 vs 决策 (c) 二进制**产物全等
  ——布局分支（级别 1..=6）不在默认路径上，默认语料证不到它。
- 单测数：lib 238 → **242**。加 4：`levels_then_render_equals_legacy_prefixes`
  （与旧函数逐条等价，含规则叠加/超长行/前导空白）、
  `level_and_literal_prefix_render_identically`（三源同页，级别 vs 字面量渲染同形）、
  `heading_level_survives_marker_merge_and_filter`（级别不丢在 T6 改写点）、
  `producer_stores_level_not_hash_literal`（走真实 `build_text_docir`，钉"级别进 IR、
  字面量不进 IR"，并验渲染幂等）。改名 4 条
  （`title_prefixes_by_numbering_heuristic`→`title_levels_by_numbering_heuristic`、
  `title_prefix_applied`→`title_levels_applied`、
  `layout_hints_drive_prefix_without_numbering`→`layout_hints_drive_levels_without_numbering`、
  `existing_hash_prefix_preserved`→`existing_hash_prefix_is_not_doubled`），删 0。

**这一格验收判据已成立**（#6 总验收里那句"块级类型+级别+bbox+span 可单测断言"）：
标题级别 + `kind` + `confidence` + `dims`（#6 第 1 步）现在都能从 IR 直接断言；
仍缺 **span**（第 4 步）与 Image/Code/Formula 等块级类别（第 5 步，依赖 #10 样本）。

**下游已解锁**：#11 的 `heading_level` 字段、#10 的 `DOC_TITLE` 判定——投影层
直接读 `Region.heading_level`，不必再反解 markdown 字面量。

**盲区照实说**：沙箱缺 13 个 gitignored real_samples，本步"逐字节不变"只对
10 条 golden + 21 个 CLI 样本 + 4 个布局开关样本成立。现网语料里若存在
"来源自带 `#` 字面量且**前导空白**"或"`#` 后无空格"这类少见的字面量形态，
两版行为都跟旧版一致（同一判据搬迁），但没被样本覆盖。

### 第 3 步已落地（2026-09-28）：`continues_prev` 进 IR，跨页合并改为"原位占位 + 标记"

**这格计划里"pass 顺带写"是错的**（不是难，是**不可能**）：`cross_page_table::run`
原先把被吸收的续页 Grid 区块**整块删掉**——续接关系只在那一刻存在于状态机的局部
变量里，pass 结束后 IR 上没有任何对象承载它。所以第 3 步的实质不是"加字段顺带写"，
而是**改 pass 的产物形状**：续页区块原位保留、打标记，由渲染层按标记跳过。

**改了什么**：

- `src/region.rs`：`Region` 加 `continues_prev: Option<bool>`（三态，对齐 MinerU
  `bool | None`）+ `is_continues_prev()`（只认 `Some(true)`）；两个构造点恒 `None`。
  没加 `with_continues_prev()` builder——写入方只有 pass 一处，直接赋值字段即可，
  加了是没人用的门面（编译器已替我抓到这一点：unused warning → 删）。
- `src/docir/passes/cross_page_table.rs`：
  - 从"`drain` 拆 Grid → 其余存回 → 定格表 push 到末尾"改成**原地遍历
    `regions.iter_mut()`**：非 Grid 区块完全不碰；被吸收的 Grid 保留**自己的原始
    grid** 并置 `continues_prev = Some(true)`；定格表**覆盖回首表区块所在位置**。
  - `pending`/`finalized` 三元组因此多带一个"页内位置"。
  - 新分支：**已带标记的区块不参与状态机、也不计入"本页有 Grid"**。不加这条，
    重复 `run` 会把已经并入首表页的行**再并一次**（行数翻倍）；加了之后本 pass
    可重复调用（单测 `run_is_idempotent_over_stubs`）。
- `src/docir/render.rs`：第 4) 阶段（网格表）加 `.filter(|r| !r.is_continues_prev())`。
  **只有这一处消费标记**，正文/成品块/TableHtml 三个阶段一字未动。

**为什么留占位而不是继续删**：MinerU 的标记挂在**续页那个块**上
（`docvortex/schema.py:506-509`、`:707`；写入点 `content/table/document.py:104`），
删掉区块就没有承载对象，投影层（#10/#11）只能看到"这张表在这一页凭空消失"。
留占位让 IR 形状与 MinerU 对齐，渲染字节不变（见下等价性证明）。

**与 MinerU 的内容口径差别（第 4 步/投影层务必读）**：MinerU 里带 `continues_prev`
的块**自己仍带正文**（它的合并发生在更后置的通路）；本仓的合并**就发生在这个 pass
里**，首表页那份是合并结果，续页标记块保留的是 producer **原始 grid**（未去重、
未并入）。→ **投影层若把标记块的 grid 当表格内容输出就会重行**，必须同样跳过，
或只取 `continues_prev` 这个事实。这条差别写在 `Region::continues_prev` 的文档里。

**零回归证据**：

- 等价性正身（不只是"跑出来一样"，而是**构造上证明两种 IR 形状渲染同形**）：
  单测 `absorbed_stub_renders_identically_to_deletion` 对同一文档跑 pass 两次，
  一份留占位、一份 `retain` 删掉占位（= 旧形状），断言 `render()` 逐字节相等，
  并钉"整篇只输出一份 `<table>`"。
- `cargo test --release` 全套 **R=0**，**303 passed / 0 failed / 1 ignored**
  （上一轮 300 → 本步 +3；lib 242 → **245**）。
- golden：**本步用了一次 UPDATE，但性质是"新增基线"，不是重基线**——新样本进清单
  时 harness 按设计报 `missing baseline snapshot`（非 UPDATE 不自动建基线），
  随后 `ANYDOC_GOLDEN_UPDATE=1` 重跑。审计方式：逐文件记录 30 条快照内容
  （UPDATE 前 `/tmp/snap_pre_final.txt`、UPDATE 后 `/tmp/snap_post2.txt`）再 `diff`：
  **只有 `tests_samples_cross_page_table.pdf.sha256` 这一行的值变化**
  （`7efab131…` → `a84d84c1…`，因测试件几何按上文重做），
  **其余 29 条既有哈希零变化** → 决策 (a) 允许的"仅标题前缀/空行变化"这条都没触发，
  字面输出确实没动。快照条数 29 → **30**（本步新增一件），去掉新增项后既有基线零漂移。
  UPDATE 后再跑一次**不带** UPDATE 的 golden：`R=0 / 1 passed`（确认新基线自洽）。
- 逐样本文本对拍：**本步二进制**（`b1f505e8…`，脚本 `/tmp/cap_step3.sh`，快照副本
  `/tmp/step3bin`）的语料 `/tmp/md_step3` 与第 2 步语料 `/tmp/md_step2f`
  逐件 `cmp`：**44/44 全等**（21 `.md` + 23 `.err`），新增 2 件即本步测试件自身。
- **这条最重要**：现有 21 个样本**没有一个**会走被改的"同列续接"分支——表格样本
  全是单页表，golden 里唯一相关的 `tests/real_samples/crosspage_table.pdf` 正是那
  13 个缺失件之一。所以"44/44 全等"在改这条分支时**不含任何被改代码的执行证据**，
  是空跑。补了入库件 `tests/samples/cross_page_table.pdf`（生成器
  `tests/gen_cross_page_table.py`，3 页：表 → 同列续表含**两行**重复表头 → 正文页），
  它**确实走合并分支并走到去重腿**（合并结果 5 行 = 3 + (3 行去重成 2 行)），
  且新旧二进制对它的产物 `cmp` **全等**（`2049e3bc…`）。该件已进 `tests/golden.rs`
  清单（`needs_ocr = false`，纯文字层，默认跑），从此这条分支有入库回归覆盖。

  **为什么页 2 要把表头印两行**（先前写成"印一行就够、去重腿自然生效"是**错的**）：
  `reconstruct_grid`（`table_grid.rs:156-157`）把每页**首行**塞进 `header` 槽、其余进
  `rows`，而 `extend_table_grid`（`table_grid.rs:268-278`）判的是
  `next.rows[0] == acc.header` 且合并时**从不读 `next.header`**。→ 续页只印一行表头时，
  那行已被 header 槽吃掉，判定**永远命不中**，走的是"直接 append"分支。实测对照：
  续页 1 行表头 与 续页顶行故意写成 `ID2/NM2/…`（不等于页 1 表头）→ 输出**逐字节相同**
  （都 5 行），证明"续页顶行无条件丢"而非"去重"。要命中的确有**两行**表头（真实跨页表
  正是"表头槽 + 又印一遍表头"这种形态）：本件 3 + (1 表头行 + 2 数据行) → 去重后 5 行。
  反向验证（防"腿恒等于 append"这种假通过）：把第二行换成非表头数据 `Z-9` → 输出变 6 行
  且 `Z-9` 保留 → 该腿在真判。

**做测试件时"撞出来的两个既有缺陷"——撤回：两个都不是缺陷，是我探针写坏的输出**
（先前本节记为"≥3 网格页表头丢字"与"正文页正文整段消失"，并猜了 `reconstruct_grid`
列模板的根因。**那个根因猜测也是错的**）。干净几何重测（当前二进制 `b1f505e8…` 与
第 2 步 `c334106d…` 各跑一遍，18 对输出**逐字节全等**）后，两条归一到一个**既有设计行为**：

`pdf/text_layer.rs` 的 `strip_furniture`（跨页重复文本 → 判页眉/页脚/水印剔除）门槛是
`pages_needed = max(3, ceil(0.6 × 总页数))`，判据是**同文本 + 同归一化位置**（x 中心、y
各 1% 箱）出现在 `>= pages_needed` 个不同页。我的合成探针把**同一行字**画在**每页同一
坐标**，正好撞上它。判别实验：

| 探针 | 形态 | 结果 |
|---|---|---|
| P2 | 3 页全表、表头**逐页相同** | 表头最左格被当家具吃掉（`<td></td><td>NM</td>…`） |
| P1 | 3 页全表、表头**逐页互异** | 表头**完整** → 门槛是"重复"不是"网格页数" |
| P3 | 6 页、3 个表页表头相同（`pages_needed=4>3`） | 三个表头**都完整** |
| K 组 | 3 个正文页同文重复 / 同形态每页文本互异 | 前者正文丢、后者**三行全留** |
| K 组 | 纯正文页同文重复、无表 | 家具判定把整层删空 → 走 `text_layer.rs:118-123` 既有回落 OCR 通路（`ANYDOC_TIMINGS` 实测每页 `ocr` 非零），文本由 OCR 重新给出 |

算术自洽（`pages_needed = max(3, ceil(0.6N))`，N=总页数）：b t b（N=3，正文 2 页 < 3）
→ 保留；b t b b（N=4，正文 3 页 ≥ 3）→ 丢；b t b t b（N=5，正文 3 页）→ 丢；
把同形态的正文逐页改成互异文本 → 全部保留。**"正文整段消失"只在文档里还剩别的非家具
页时才是净损失**——整篇都是家具时回落 OCR，文本反而回来（代价是多付一次 OCR）。

**这里有一个真实的、值得单独记的产品面**（不是本步引入、也不由本步修）：真实跨页表的
表头行天然"逐页同文本同位置"，所以**总页数 ≤ 5**（`pages_needed` 仍是 3）的短文档里，
表头跨 3 页重复就会被判成页眉/水印而丢字。这与 MinerU 的家具剔除同族，但阈值口径
（1% 位置箱 + 3 页下限）值得在 **#10 的"页眉页脚不丢弃而是标注 + 可选输出"**那一并
复核（该子项在本仓已有记录，见 #10 段末"例外"条）。本步不动它。

测试件因此刻意取"3 页、表头只重复 2 页、正文行全文档仅出现 1 次"——实测表头四格完整，
家具判定的产物不会混进"跨页合并"的断言面。再加页就会撞上门槛。

**盲区**：`continues_prev` 目前**只有 IR 层消费者（渲染跳过）没有输出面**——按决策 (b)
本步不新增 CLI 输出，投影层（#10 content_list / #11 middle_json）才是它的使用方。
真实跨页表（`tests/real_samples/crosspage_table.pdf`）在沙箱里仍然缺件，现网语料的
合并行为要等有样本的环境补跑。

### 第 4 步已落地（2026-09-28）：`Span`/`spans` 进 IR，pdf-inspector 样式证据直产

**改了什么**：

- `src/region.rs`：`SpanStyles`（bold/italic/underline/strikethrough/superscript/
  subscript 六位布尔 + `is_plain()`）与 `Span { text, styles }`（`new`/`plain`
  两个构造，`plain` 是全零样式的退化形态）；`Region` 加 `spans: Vec<Span>`
  （**空 vec = 无样式信息来源**）+ `with_spans()` builder。字段语义写在
  `Region::spans` 文档：**渲染层不消费**，消费方是未来的投影层（#10/#11）。
- `src/pdf/text_layer.rs`：`build_spans()`——按 pdf-inspector `TextItem` 的
  样式证据切段（相邻同键合并），接线 `push_line_region`。样式键取自
  `is_bold/is_italic/is_underline/is_strikeout` 与 `baseline_shift`
  （`> 0` → superscript、`< 0` → subscript）。
- `src/ofd/text_layer.rs`：`to_regions()` 每行产单 `Span::plain`——ofd-core
  拿不到样式证据，单 span 全零样式 = "有信息但无装饰"，与空 vec 的
  "根本没有 span 来源"是两种可区分的状态。
- `src/gfm_adapter.rs`：OCR 通路 Region 构造点加单 plain span（OCR 行同样无
  样式证据）。

**两条设计决策**（第 5 步/投影层务必读）：

1. **双层真相分工**：`Region.text` 是**渲染/文本真相**（来自 `text_plain`，
   含 `<sup>/<sub>` 标签与完整插空规则）；`spans` 是**样式/run 边界真相**
   （旁路信息）。本步渲染层零消费 spans → 输出逐字节不变是**结构性承诺**
   （渲染代码一字未动），不是"碰巧跑出来一样"。
2. **刻意不复刻 `text_plain` 的完整插空规则**（避免双源漂移——两套插空逻辑
   各自演化迟早分叉）：跨段空格只做一条几何判定——样式切换处 gap ≥ 0.2em 且
   两侧非空白 → 空格归前段尾。单测
   `spans_join_matches_text_ignoring_tags_and_spaces` 钉住弱一致性契约：
   spans 拼接（去标签、去插空空格）== text。强等值（含标签与空格的逐字符
   对应）需要 text/spans 同源重写，不在本步。

**零回归证据**：

- `cargo test --release` 全套 **R=0，316 passed / 0 failed / 1 ignored**
  （上一轮 303 → 本步 **+13**：region 4 + pdf 8 + ofd 1；lib 245 → **258**）。
- golden：`ANYDOC_GOLDEN_OCR=1 cargo test --test golden` **未用 UPDATE**：
  **11 checked**（第 3 步新增 `cross_page_table.pdf` 后 10 → 11）+ 13 skipped
  （real_samples 缺件），1 passed，既有快照零漂移。
- CLI 对拍（**重建基线法**——此前"21 md + 23 err"语料目录已不在 /tmp，改用
  git stash 前后双二进制对同一语料重跑）：`git stash` → 改前二进制 →
  `tests/samples` 全量 26 件（22 成功：19 个 md，其中 image/text/image_table
  三对 ofd/pdf 同名覆盖；4 失败：corrupt×2、encrypted×2，失败件只打 stderr
  不落 err 文件）→ `git stash pop` → 改后二进制重跑 → **19/19 md 逐字节
  全等 + 4/4 失败 stderr 逐字节全等**。
- **盲区照实说**：本步 spans 的证据面只有单测（`push_line_region_attaches_spans`
  全链接线 + 切段规则 8 件），CLI/输出面零变化是"渲染不消费"的结构保证而非
  现网样本验证；13 个 gitignored real_samples 仍缺，真实粗体/斜体/上下标语料的
  样式还原质量要等有样本的环境补跑（#10 投影层落地时一并审）。

### 第 5 步已落地（2026-09-28，枚举先行）+ #10 例外项：`Noise`/`Footnote` 有 producer，页面家具从"无痕丢弃"改"标注 + 可选输出"

**改了什么**：

- `src/region.rs`：`RegionKind` 扩 7 个变体——
  - **占位五类** `Image`/`Code`/`Formula`/`Index`/`Aside`：producer 未产
    （`#[allow(dead_code)]` 注明消费方是 #10 渲染分支），枚举先行让 #10 的
    投影层与单测有可断言的落点；渲染层对它们零消费（手工构造也不输出，
    单测 `placeholder_variants_never_render` 钉住）。
  - **本步有 producer 的两类**：`Footnote`（脚注独立成 kind，不与页脚混——
    oar 的 `is_footer()` 把 `Footnote` 并进页脚口径，但 MinerU 13 项里
    `PAGE_FOOTNOTE` 是独立类型）与 `Noise(NoiseKind)`（细分
    Header/Footer/PageNumber/Seal）。
- `src/gfm_adapter.rs`：OCR 通路收集层的家具处理从 **`continue` 丢弃改为
  分流**——`furniture_kind_of()` 按版面元素类型给 kind（Header/HeaderImage →
  Header、Footer/FooterImage → Footer、Number → PageNumber、Seal → Seal、
  Footnote → `RegionKind::Footnote`），命中文本收进 `furniture` 数组带 bbox
  与 confidence 进 IR（追加在正文/表格之后，顺序无阅读序保证）。
  **`order_structure` 一字未动**——正文 regions 输入不变，三重过滤的阅读序
  语义原样（`blocks.rs` 的 NOISE_TYPES 6 类 + leftover 排除照旧）。
- `src/docir/render.rs`：`render()` 委托 `render_with_furniture(doc, false)`；
  `emit = true` 时每页段末追加注释行——`<!-- header: … -->` / `<!-- footer: … -->`
  / `<!-- page-number: … -->` / `<!-- seal: … -->` / `<!-- footnote: … -->`，
  按 `y_min` 升序（页眉在前页脚在后，det 顺序不作保证），文本中的 `-->`
  替换为 `->`（防提前终止 HTML 注释；显示用标注，不做原文保真）。
- 开关：env `ANYDOC_EMIT_FURNITURE`（存在即开，与 `ANYDOC_HEADINGS_LAYOUT`
  同族语义），接线点在 `gfm_adapter::to_markdown`。**不用 CLI flag /
  RenderConfig 字段**的理由：注释形态是过渡（正式出口是 #10/#11 的
  content_list v2 投影，`PAGE_HEADER`/`PAGE_FOOTER`/`PAGE_NUMBER` 独立
  item），不值得为它穿透 6 层签名；文字层通路无家具 producer，env 天然
  只作用于 OCR 通路。
- `README.md`：环境变量表新增一行。

**为什么"注释形态"而不是别的**：GFM 合法、不污染可见 markdown 文本、
可 grep；MinerU 的 content_list v2 把家具落成独立 item 的语义在投影层
（#10/#11）才是正式对齐点，markdown 注释只是过渡期的可观察出口。

**零回归证据**：

- `cargo test --release` 全套 **R=0，322 passed / 0 failed / 1 ignored**
  （上一轮 316 → 本步 **+6**：gfm_adapter 2 + render 4；lib 258 → **264**）。
- golden：`ANYDOC_GOLDEN_OCR=1` **未用 UPDATE**，**11 checked** + 13 skipped，
  1 passed——默认路径渲染层零消费 Noise/Footnote，输出逐字节不变（结构性
  承诺：`render()` 只是把 `false` 传进 `render_with_furniture`）。
- CLI 对拍（沿用批次 A 的重建基线法）：`/tmp/par-before`（批次 A 改前二进制
  × 26 件入库样本）对批次 B 改后二进制重跑 → **19/19 md + 4/4 失败 stderr
  逐字节全等**（跨两个批次的双重验证：批次 A 基线 → 批次 B 不漂移）。
- 开关冒烟（seal_scan.pdf，含一枚章）：关 = 与批次 A 基线逐字节一致；
  `ANYDOC_EMIT_FURNITURE=1` 后**多出一行 `<!-- seal: 专用章 -->`**——章 bbox
  内 det 读出的文本此前被三重过滤无痕丢弃，现在开关下可见。它与
  `seal_pass` 的 `【印章】专用章` 行并存（两条独立通路：版面 `Seal` bbox
  内的散落 OCR 文本 vs 印章专用检测+识别），各自语义见 README 两行。

**盲区照实说**：注释行的真实语料覆盖只有 seal 一样本（页眉/页脚/页码/
脚注的注释行只在合成单测里见过）——13 个 real_samples（公文类天然带
页眉页脚页码）补进来后要实跑一遍开关，核对 y 排序与注释内容符合阅读
习惯。`Footnote` 的 bbox 内文本在 `order_structure` 里**不算**噪声
（`NOISE_TYPES` 不含 Footnote）——本步靠收集层分流，若未来把收集层
分流撤掉，脚注会漏进正文，这条耦合写在 `furniture_kind_of` 文档里。


### 风险（务必先读）

解耦标题前缀会动到**现网字节一致契约**（golden 10 样本 / batch 6 样本守护）。

**原写的缓解手法已随决策 (c) 失效**（本节此前说"照搬 `ANYDOC_RICH_TEXT` 的
判定视图/渲染视图分离"——那个判定视图已删除，照搬无从谈起）。第 2 步改用
这些手法兜底：

- **一次切干净 + 显式重基线**（决策 a）：`heading_level` 进 `Region`、`#` 前缀
  下移到 `docir/render.rs`，producer 只设字段。
- **逐条 diff 每条快照**：允许的变化**只**有标题前缀本身与其前后空行；任何正文
  字符差异判回归。快照是 16 位 hash 看不到内容，所以还要另建改前/改后的
  CLI markdown 语料做文本对拍（同第 1、决策 (c) 两步的做法）。
  → 第 2 步实测：连"只允许标题类变化"这个宽容档都没用上，语料 21/21 全等。
- **机制性单测钉"级别信息来源"**：`heading_levels`/`reading_order::title_level`
  产出的级别可直接断言，不靠 markdown 字面反推。
- **盲区照实说**：沙箱缺 13 个 gitignored real_samples，第 2 步的 UPDATE 只能覆盖
  这 10 条快照；现网语料的等价性要在有样本的环境补跑。
- 新增结构化输出时**不动** markdown 渲染器的任何字面输出，用"同一 IR 两个
  renderer"来证明等价（新 renderer 的 markdown 输出与旧通路逐字节等值，
  是 #6 的硬验收）。

### 验收判据

- IR 层面：块级类型 + 标题级别 + bbox + span 可被单测直接断言（当前不可）；
- 第 1 步（页尺寸入 IR）**单独零回归**：`cargo test --release --test golden`
  （`ANYDOC_GOLDEN_OCR=1`）仍 "10 checked" 且**不用** `ANYDOC_GOLDEN_UPDATE`
  ——这一步渲染层不消费新字段，输出必须逐字节不变；
- 第 2 步（标题前缀下移）**允许并要求重基线**（决策 a）：`ANYDOC_GOLDEN_UPDATE`
  后逐条 diff 快照，变化**只**允许出现在标题前缀与其前后空行；任何正文字符变化
  即判回归。原"未使用 UPDATE"的写法作废；
  **第 2 步实际没有用 UPDATE**（输出逐字节不变）。作废的那句在本步重新成立，但它是
  **禁令**而非预期：今后任何一步若真改了字面输出，仍照决策 (a) 走 UPDATE + 逐条 diff。
- 等价证明：markdown renderer 改造前后对 6 个 batch 样本 hash 全等（第 1 步）；
- 不新增必须项：默认路径（不请求结构化输出）分配/耗时无可见退化。

---

## #7 无线表格结构模型（unet）未接线

状态：**第 0 步已做完并出结论；两条候选路都不采用为默认**（2026-09-27）。
本条目仍**未完成**——但"未完成"的含义变了：不是"没数据所以没做"，而是
**"数据说这两条都不该进默认路径"**。已落一个默认关闭的 A/B 开关
（`ANYDOC_WIRELESS_CELLS`）供现网语料复测。

**结论一句话**：(ii) 技术上不兼容（要 500+ 行新 decoder）；(i) 可实现**且已实现**，
但**没有任何一版数据支持把它放进默认路径**——入库件上持平（还多一层 `<tbody>`）、
紧行高件上明显做坏，同时确定地多付 129MB 模型与每页一次检测。现状（slanet_plus
当通用兜底）仍是三条路里最好的一条。

### 实测数据（真 CLI 通路，`--dpi 150`）

两组测试件都跑过：`/tmp/wtab/`（#7 第 0 步合成件，18pt 字 / 行高 45px）与
入库版 `tests/samples/{wireless_span,wireless_simple,wired_table}.pdf`
（`tests/gen_wireless_tables.py` 生成，24pt 字 / 行高 60px，页尺寸按 150dpi
反算使像素 1:1）。

| 测试件 | 真值 | 现状 OFF（slanet_plus 兜底） | `ANYDOC_WIRELESS_CELLS=1`（cells→HTML） |
|---|---|---|---|
| `wireless_simple` | 3×3 无 span | ✅ 完全正确 | ✅ 正确，但**多包一层 `<tbody>`**（输出字节变了） |
| `wireless_span`（入库版） | Header1 colspan=2、Merged rowspan=2 | ✅ 全对 | ✅ 行列与两个 span 也对，同样只多 `<tbody>` |
| `wireless_span`（第 0 步版，行高更紧） | 同上 | ✅ 全对 | ❌ `Header1 colspan=3`（错）、Data3/Data5/Data7 **丢失**、空 `<tr></tr>`、`Merged rowspan=3` |
| `wired_table` | 4×3 有线 | ⚠️ 前 4 行全对 + **1 行空 `<td></td>` 噪声** | ⚠️ 与 OFF **逐字节一致**（空行噪声不来自这条通路） |

**判读**：cells→HTML **最好情况也只是追平**现状（还多出一个改变输出的 `<tbody>`），
在行高更紧的版式上则**明确做坏**——即它对版面几何敏感，而现状不敏感。
叠加"默认档多加载 129MB + 每页多一次单元格检测"的确定成本 → **不进默认路径**，
只留 `ANYDOC_WIRELESS_CELLS` 供现网语料自行 A/B。既有验收判据（"无线表结构
不一致数**下降**"）在两组件上都不成立（一组持平、一组变差）。

**顺带量到的一件事（记在这里，别丢）**：无线表的 span 归属对**版面几何**敏感——
我按同一段网格代码复现测试件时，18pt 字 / 行高 45px 的版本在**现状通路**上就会把
`Merged` 的 rowspan 判丢（降级成普通格），换 24pt / 行高 60px 才稳定正确；而 #7
第 0 步那份 18pt 件（页尺寸不同）在现状通路上是对的。也就是说这类合成件的
**字号、行高、页缩放三者共同决定结论**，任何一项漂移都会让"结构正确率"变成噪声。
故入库版把几何显式钉死（页尺寸按 150dpi 反算使 `--dpi 150` 像素 1:1、画布尺寸写死
不随内容推算），理由写进 `tests/gen_wireless_tables.py` 头部。
另一条独立观察：有线表"多一行空 `<td>`"的噪声在 OFF/ON 下逐字节相同 → 根因在
版面/表格边界判定，不在结构通路，另案（与 #8 一并看）。

### (ii) UNet 接线的取证（子代理源码链，2026-09-27）

`with_wireless_table_structure` 槽位硬绑 `SLANetWirelessAdapterBuilder`
（`structure.rs:1057-1075`），其解码期望 `SLANetModelOutput{structure_logits
[batch,seq,vocab], bbox_preds[batch,seq,8], shape_info}`
（`slanet.rs:26-34`）；而 `unet.onnx` 实测 I/O 是
`input [b,3,H,W] float32 → output [1,b,H,W] float64`，**输出是像素级分割图**。
两者不兼容，接进去要新增 adapter + decoder + 连通域分析（估 ~500 行），
不是"换个模型"。(ii) **判死**，除非将来 UNet 通路本身成为对齐硬需求。

### 已落地（本轮）

- `src/models.rs`：`ModelSpec.wireless_cell_det` 字段（仅 mineru-basic 有候选件，
  小档留空）+ `wireless_cells_wanted()`（`ANYDOC_WIRELESS_CELLS` 存在即开）。
- `src/ocr_engine.rs`：`build_analyzer` 按开关挂
  `with_wireless_table_cell_detection(...)` + `use_wireless_table_cells_trans_to_html(true)`；
  `EngineKey` 新增 `wireless_cells` 位（两种结构不能共用一个引擎缓存）。
  开关生效时打一行 stderr 说明——129MB 的加载不声不响最难查。
- 该件**不计入** `MINERU_ASSETS`：那是"默认档首跑必需"的承诺，开关关时不该承诺，
  也不该让预检多算 129MB。资产本身在注册表内（`registry.rs:104`，129,331,821 B），
  开关给了就能 auto-download。
- `examples/wireless_table_probe.rs`（子代理产物）**已删除**：它自建 builder 时
  没挂 `.with_ocr()`，对三个测试件全部输出 `<no table>`，量的不是我们的管线，
  留着会误导后续对比。A/B 一律走真 CLI（上表数据即此法所得）。

**问题 → 动作**：无线表结构识别不如 MinerU basic → 先**二选一比测**（上游 cells→HTML vs UNet 接线），再谈实现。

**取证事实（保留，供后续复核）**：

1. MinerU 的 basic 必需件含 `unet_structure: Table/unet.onnx`
   （`mineru/model/registry.py:42-64`，ONNX 与 Torch 两个仓库都有此件）；
2. 分类器判据：`cls_label == WiredTable`，或 `WirelessTable 且 cls_score < 0.9`
   → 走**有线**模型（`tables.py:885-888`）——即"不确定时偏有线"；
3. **分类失败/未获结果的表默认按 wireless 处理**（`tables.py:871` 注释），随后
   无线 batch 跑 `wireless_table_model.batch_predict`（`tables.py:875`）；
4. 本仓现状：恒 `with_table_structure_recognition(slanet_plus, "wireless")`
   （`src/ocr_engine.rs:399`，注释说明用 "wireless" 标签当通用兜底防 config_error
   整页失败），`unet` 在**整个仓库零引用**，分类器输出的 Wired/Wireless 分支
   未被区分使用；
5. 资产：本机 `/data/models/mineru-ocr/unet.onnx` 存在（8,335,007 B），
   但 `unet` **不在本仓 ModelScope 注册表内**（`oar-ocr-core/.../download/registry.rs`
   全部 98 条 `Entry` 里 grep `unet` 不中）→ 与公式件同一类"永不 auto-download"
   资产，接线时要一并决定来路。

**修正我口头汇报过的一处口径**：分类器结果**不决定"跑哪个模型"**。MinerU
`tables.py:869-894` 的实际顺序是"跑 cls batch → **再**跑 wireless batch →
最后用 cls 结果在两份 HTML 里选一份"（0.9 是无线件的置信度门限）；即
"分类失败按 wireless"的含义是"两份 HTML 里 wireless 那份被选中"，而不是
"改跑别的模型"。本仓是"一个 adapter 兜所有 TableType"，`Unknown` 分支
（`table_analyzer.rs:431-435`）正是我们注释里依赖的那条兜底路径。

**实施前必须先核一件事（别猜）**：上游 oar-ocr 的 wireless 分支语义与 MinerU 不同源。
`structure.rs:1017` 显示 `TableType::Wireless → SLANetWirelessAdapterBuilder`——
即**上游的"无线"仍是 SLANet 系**，不是 UNet；而 `use_e2e_wireless_table_rec`
（默认 true，`structure.rs:228-233` 注释 "wired=false (use cell detection),
wireless=true (E2E mode)"）与 `with_wired_table_cell_detection` /
`with_wireless_table_cell_detection`（`structure.rs:553-575`）、
`use_wireless_table_cells_trans_to_html` 是另一条 **单元格检测 → cells→HTML**
通路（`table_analyzer.rs:149 table_cells_to_html_structure`、`:441`、`:536`）。
→ **候选解法至少两条，第 0 步是先把它们量清楚再选**：
  - (i) 走上游 cells→HTML：`rt-detr-l_wireless_table_cell_det.onnx`
    **已在注册表**（`registry.rs:104`，129,331,821 B），无需新资产来路，但代价是
    129MB 模型 + "单元格检测"与 MinerU 的"UNet 结构"是**不同技术方案**，
    对齐的是能力而非实现；
  - (ii) 把 unet.onnx 接进 `with_wireless_table_structure`——**先确认该槽位是否
    吃得下 UNet**（`structure.rs:1057-1075` 用 `SLANetWirelessAdapterBuilder`
    解码 structure tokens，UNet 输出是单元格框图，二者 decode 逻辑大概率不兼容）；
    若确实不兼容，(ii) 要在 vendored 层加 UNet decoder，工作量一个数量级更大。
  选路依据：先用同一张无线表分别跑 (i) 与现状，比结构正确率；
  **别默认"复刻 MinerU 就必须用 UNet"**——我们的目标是输出质量对齐。

**验收判据**：`tests/samples/` 需新增（或从 real_samples 选）至少一张**无线表/合并
单元格表**样本；用 `ANYDOC_DUMP_DIR` 对拍本仓表格 HTML vs MinerU basic 的表格 HTML，
结构（行列/span）不一致数**下降**为唯一通过条件；有线表样本输出逐字节不变
（防把已有精度做退）。

---

## #8 表格方向的视觉兜底分类器缺失

状态：**已实现为默认关闭的 `ANYDOC_TABLE_ORI`，第 0 步 + A/B 已出结论（2026-09-27）**。
差距**是真的**（不是我早期记的"basic 无方向矫正"那么回事），但**不进默认路径**，
理由与 #7 不同：#7 是"增益未证"，#8 是"增益已证、但缺一个零副作用的机制保证"。

### 先更正本条目早期的一处误判（重要，别再抄错）

本节原文写"本仓 `mineru-basic` 档按 MinerU 口径**关闭**页面方向矫正……这是刻意的
对齐"，并据此把 #8 归入"要不要做"。**方向是对的，理由错了一半**：MinerU 有两个
**不同**的槽位，之前把它们混为一谈了——

- **页面级**方向矫正（转整页）：MinerU basic（=hybrid effort medium）**确实没有**，
  所以我们 `doc_ori: ""` 是对的，别顺手打开；
- **表格级**方向矫正（只转"表格裁剪图"、在结构识别之前）：MinerU **确实跑**，
  `backend/analysis/pdf/window.py:389` `if effort in ["flash", "medium", "high"]`
  里就含 medium（`parser/tier.py:14` basic→medium）。

→ 所以 #8 不是"要不要额外做一个 MinerU 没有的东西"，而是**"MinerU 有、我们没有"
的真缺口**。`src/orientation.rs` 那套 PDF 文本行投票**盖不住**这条——它只在有文字层
时投票，且投的是**整页/块级朝向**，纯图片扫描页里被版面框出来的旋转表拿不到这道信号。

### A/B 实测（真 CLI，`--dpi 150`，OAR_HOME=/root/.oar）

测试件：`tests/gen_table_ori.py` 生成 `table_upright.pdf` / `table_rot90.pdf`（入库，
同一张 3×4 有线表只差朝向、逐像素同源），180°/270° 两版只在 /tmp 生成不入库。
真值 = `wired_table.pdf` 那张表，故判据可复用 `wireless_table.rs` 的网格断言口径。

| 测试件 | OFF（现状） | ON（`ANYDOC_TABLE_ORI`） |
|---|---|---|
| `table_upright`（0°） | 正确 | **逐字节相同** |
| `table_rot90` | ❌ **转置残局**：3 行 × 5 列，单元格被劈成 `C` / `herry` / `B` / `nana`，`$` 单独成格 | ✅ 每行 3 格、5 行、表头 `Item`/`Quantity` 就位（rec 仍有瑕疵：`Price`→`rice`） |
| `table_rot180`（/tmp） | ❌ 行列顺序反、`Appple`/`Banaana` 粘连、首列变金额 | ✅ 行列顺序正确、`Price` 保住（rec 瑕疵同类） |
| `table_rot270`（/tmp） | ❌ 转置成 5 列（`Item/Appple/Bannana/Cheery` 挤进一行） | ✅ 正确 3 列 × 4 行 |
| 既有表样本 `wired_table` / `wireless_span` / `wireless_simple` / `image_table.pdf` / `rotated_table.pdf`（文字层件） | — | **全部逐字节相同**（6 件，含非 OCR 通路） |
| `multipage.pdf`（12 页）耗时 | 15.65 / 15.81s | 15.92 / 15.38s（**噪声内**） |

**结论**：三个角度上 ON **严格更好或持平，无一处变坏**；正常表零影响；开销测不出。
断言只钉"网格形状 + 表头存在"，**不逐格钉文本**——rec 对旋转后文字的精度是另一笔债
（属 #7/#9 范畴），钉进来会把两条债焊成一个易碎的测试。

一处观察但**不下结论**：rot90 的 OFF 输出带 `<tbody>`、ON 不带。这与 #7 里
cells→HTML"多包一层 `<tbody>`"现象同源（`<tbody>` 的有无似乎在区分"E2E 结构通路"
与"兜底/重拼通路"），但样本太少（n=1），**不要据此推断通路选择规则**。

### 为什么不进默认路径（与 #10b 的对比才是关键）

#10b 敢默认开，是因为有**机制保证**：`seal_pass` 在页面无 `Seal` 版面元素时早退，
模型连加载都不加载 → "不含章的文档零影响"是可证的，不是实测出来的。

#8 没有这个机制。`with_table_orientation` 一旦挂上，**每个表格裁剪都要过一遍分类器**，
正常表靠的是"分类器判 0° → `apply_orientation_from_class_id` 不旋转"
（`preprocess.rs:128-133`，class_id=0 走 `_ => image` 原样返回）。也就是说它的
"零影响"依赖**分类器不误判**，而误判的后果是**把一张本来正确的表转歪**——
静默降级，比不修更糟。

而这里恰恰验不全：`tests/real_samples/` 13 件是 gitignored、本沙箱不存在，
golden 的 OCR 基线只能重跑仓内 10 件。我没有在现网语料上量过误判率，
不能拿"6 件合成/仓内件全等"外推成"对所有文档安全"。

叠加第二条：`pp-lcnet_x1_0_doc_ori.onnx` 当前**不在** `MINERU_ASSETS`（basic 的
`doc_ori` 是空串）。默认开就必须把它加进必需件，默认档首跑承诺从 7 件/≈240MB
变成 8 件/≈247MB，`mineru_assets_match_spec` 与离线包脚本都得跟着改——
为一个未量误判率的能力，抬高"首跑必需"的门槛，不值。

→ **落法**：默认关闭的 `ANYDOC_TABLE_ORI` 开关（`EngineKey` 加 `table_ori` 位，
生效时打一行 stderr 说明），`tests/table_orientation.rs` 三条契约钉死
"正常表逐字节不变 + 旋转表网格转正 + 开关有痕迹"。
**翻默认的唯一前置**：拿 `real_samples` 跑一轮 OFF/ON 对拍，确认"旋转表修复数 > 0
且正常表变化数 = 0"，同时把 doc_ori 加进 `MINERU_ASSETS` 并重基线 golden。

### 子代理取证报告里被推翻的 4 处（别照着它开工）

1. "改动在 `src/main.rs`"——**错**，构建 analyzer 的地方是
   `src/ocr_engine.rs::build_analyzer`（本票接线即在此）。
2. "`/root/.oar` 为空、模型只在 `/data/models/mineru-ocr/`"——**两处都错**：
   `/root/.oar` 有 15 个文件且**含** `pp-lcnet_x1_0_doc_ori.onnx`（6,787,248 B）；
   `/data/models/mineru-ocr/` 里**没有**这个文件（只有 `table_cls`）。
3. "`pp-lcnet_x1_0_table_cls.onnx` 是方向分类器还是有线/无线分类器待查"——已确认
   是**有线/无线**分类器，且我们早已接线（`ocr_engine.rs:401`），与本票无关；
   本票用的方向件是 `registry.rs:64` 那张 `doc_ori`（6.8MB，在注册表内可
   auto-download）。
4. "改动面 60–110 行、无阻塞、可直接进第 1 步"——**低估**：真正的门槛不是接线
   （接线确实只有 3 行），而是上面那节"误判率无法在现网语料外验证"。

### 原始取证（保留，已按上文更正）

MinerU 表格三阶段第一步是"先 PDF 原生文本行投票，**不足时**才调视觉方向模型"
（`mineru/backend/analysis/pdf/tables.py:530-559` `_apply_table_orientations`，
`window.py:389` 按 effort 触发；模型
`model/table/cls/mineru_table_ori_cls.py`）。本仓 `src/orientation.rs` 实现了同一套
PDF 线投票口径（046b08a ②），但**没有视觉兜底** → 纯图片型扫描件里旋转 90°/180°
的表格没有第二道信号（本票要补的正是这一条）。

**MinerU 侧的门控与阈值**（`mineru_table_ori_cls.py:22` 等，我们**未**复刻，记此备查）：
- 候选门控：竖框（宽高比 < 0.8）数 ≥ 总框数 28% 且 ≥ 3 个才进入多角度评分
  （`ROTATED_TEXT_*`，`:13-16`）；
- 角度来源：用 **OCR rec 置信度评分**选角度，不是独立视觉分类器
  （`ORIENTATION_SCORE_*`，`:18-21`：0° 分 ≥0.9 直接判 0°，其他角度与 0° 差 <0.08
  时保守保持 0°）；候选角只有 **0/90/270**，**没有 180°**（`ORIENTATION_SCORE_LABELS`
  `:22`）；
- 有效识别结果 <5 条记 0 分（投票不足判据）。
→ **我们接的是另一套技术方案**：oar-ocr 的 `DocumentOrientationAdapter`
（PP-LCNet 视觉分类器，224×224 输入、4 类 0/90/180/270，`structure.rs:437` 注释
"uses the same model as document orientation detection"）。
**对齐的是能力，不是实现**（与 #7 那条 cells→HTML 同一处境）。差异有两处值得留意：
它有"保守保持 0°"的门控而没有；我们能处理 180° 而它不能（实测 180° 上我们确实修对了）。
若要翻默认，这组阈值是现成的调参参考，不必自己发明。

**上游接线事实**（已核，非推断）：`with_table_orientation(model_source)`
在 `third_party/oar-ocr/src/oarocr/structure.rs:442`，与页面级
`with_document_orientation`（`:379`）是**两个独立槽位**；表级槽位在
`structure.rs:920-925` 建 `DocumentOrientationAdapter`，消费点在
`table_analyzer.rs:352-384`（每个表格裁剪调 `correct_image_orientation`，
失败则 warn 后**原图继续**，不整页失败）；0° 判定走
`preprocess.rs:128-133` 的 `_ => image` 分支**不旋转**。

**本票验收判据**（已满足，逐条对应上文实测表）：合成样本表块旋转 90°/180°/270°
在 `--dpi 150` 下网格重建正确 ✅；非旋转页与全部既有表样本输出逐字节不变 ✅
（`tests/table_orientation.rs` 三条契约守护）。**未完成的部分**：现网语料
（`tests/real_samples/`，gitignored）上的误判率未量——这是翻默认的唯一门槛。

---

## #9 表内对象吸收 + 行内公式 / 公式编号

状态：**第 0 步已完成（2026-09-27，两侧同件对拍），结论是"检测侧已具备、装配侧丢信息"**。
本条目原写的"表内公式/图丢"方向对，但**根因不在公式识别**——MFD/MFR 全在跑且结果与
MinerU basic 逐字一致；丢在**我们把上游已经拼好的 `LayoutElement.text` 扔了**。

### 第 0 步对拍（真 CLI vs 真 MinerU basic，同一张图页）

测试件 `tests/samples/formula_mixed.pdf`（`tests/gen_formula_mixed.py` 生成，1000×620px
图像页 → 480×297.6pt，`--dpi 150` 像素 1:1；无文字层已用 PyMuPDF 核实 `len(text)==0`）。
一页五个观测点：行内公式 / 带编号行间公式 / 无编号行间公式 / 表内公式 / 表内图。
对照侧：本机 `mineru parse --tier basic`（本地 server，非远程），故两边**同流程同模型**。

| 观测点 | MinerU basic 输出 | 本仓输出 | 判定 |
|---|---|---|---|
| 行内公式 | `The relation $E = m c ^ { 2 }$ links mass and energy here.`（**留在行内**） | `The relationE=m c^{2}` 换行 + `links mass and energy here.` 与**下一段粘连** | ❌ **真缺口** |
| 行间公式 | `$$ V = IR $$`（有定界符） | `V=I R`（裸 LaTeX，无 `$$`） | ❌ 渲染缺定界符 |
| 公式编号 | 并进公式块 `\tag{1}`，**不独立成行** | `(1)` 独立成一行 | ❌ 缺编号合并 |
| 表内公式 | `$\overline { { f ( x ) } } = x ^ { 2 } + 1$`（**一份**） | `\overline{{f(x)}}=x^{2}+1<br/>$\overline{{f(x)}}=x^{2}+1$`（**同一格两份**） | ❌ 缺去重/取 LaTeX 形态 |
| 表内图 | 该格**空** | 该格**空** | ✅ 持平（且见下方"没量到"的说明） |
| MFR 识别质量 | `\overline{{f(x)}}=x^{2}+1`（把表格横线读成上划线） | **完全相同**的 `\overline{{f(x)}}` | ✅ 同源，非差距 |

**"表内图"这一格实际没量到**（诚实记录，别把持平当结论）：格内放的曲线位图被**两家
的版面模型都判成 `inline_formula`**——我们 dump 里是 `Formula/inline_formula` +
latex `\sim`，MinerU 直接吐 `$\smile$`。第二版换成灰度渐变块后该格不再出公式、
两侧都空，但仍没触发"表内图吸收"。→ **本票的"图那半仍未测到**，需要一个版面模型
明确判为 `Image` 且落在表格 bbox 内的样本（现成件 `image_table.pdf` 是"整页当图"，
不是"表格里有图"，不能替代）。

### 关键取证：信息在上游就有，是我们丢的

`ANYDOC_DUMP_DIR=/tmp/fx_dump3` 的逐页 JSON（本仓自己的中间产物）显示：

- `layout_elements[0]`（Text）的 `text` = `'The relation $E=m c^{2}$ links mass and energy here.'`
  ——**上游已经把行内公式拼回原句**（`third_party/oar-ocr/.../structure.rs` 的
  inline-formula stitching，正是本条目原来引的 `structure.rs:2816-2824` 那段）；
- 同一页 `layout_elements` 带完整 `label`：`inline_formula` / `display_formula` /
  `formula_number`（`text='$$(1)$$'`）/ `table` / `text`，且 `order_index` 1..10 齐；
- `text_regions` 里公式是**额外**的行（region 7 `E=m c^{2}`、region 9 `(1)`、
  region 11 `\overline{{f(x)}}=x^{2}+1`，conf 全 1.00）。

而本仓装配路径（`src/gfm_adapter.rs:236-257`）的正文行**只来自 `text_regions`**，
`LayoutElement.text` 目前仅被 Seal（`#10b`）与 title 前缀两处消费。于是：
拼接好的整句被弃用 → 行内公式以"孤立行"形式落进正文 → 段落合并（`merge_into_paragraphs`）
按 y 邻近把它和邻居焊成 `The relationE=m c^{2}` 与 `links…here.Paragraph continues…`。
**这不是模型差距，是我们自己少用了一路已经算好的信息。**

### 由此得出的最小修法（按性价比排序，尚未开工）

1. **正文行优先取带 `label` 的 Text 元素文本**（约等于"用 stitching 后的句子"），
   det/rec 行只在元素无 text 时兜底 → 一次修好 行内公式掉行 + 段落粘连，且顺带
   让 `order_index` 真正生效。风险：改的是 OCR 通路正文装配，**golden 基线必然要重跑**
   （`tests/samples/image*.pdf`、`multipage.pdf` 等 OCR 件都在契约内）。
2. **display formula 加 `$$…$$`**：纯渲染层，按 `label=display_formula` 分流。
3. **公式编号并入公式块**：MinerU 有 `optimize_hybrid_formula_number_blocks`
   （`formulas.py:75-102`）；我们已有 `FormulaNumber` 元素与 `$$…$$` 载荷，缺的是
   "就近并入 + 不再独立成行"的几何判据（同 y 带、x 在右侧）。
4. **表内单元格去重**：`cell_texts` 已是 `X<br/>$X$` 双份，取一份（优先 `$` 形态）。
   注意这与 #6 的 `TableBlock.cell_merge` / 内容表示是同一块地皮，动之前先看 #6 的
   三个待决断。
5. **表内图吸收**：仍**不做**——第 0 步没量到 MinerU basic（无 VLM）在表内图上到底
   输出什么（本条目原引的 `tables.py:714-715` base64 吸收属哪条通路未证），
   先补一个"版面判 Image 且在表 bbox 内"的样本再定。

**验收判据**（第 0 步后收紧为可测形式）：`formula_mixed.pdf` 上行内公式**在行内**
（输出含 `The relation $E=m c^{2}$ links` 形态、且 `links mass and energy here.` 不与他段
粘连）；行间公式带 `$$`；`(1)` 不独立成行；表内公式单元格只有一份；既有 OCR golden
逐条重基线（不是"不变"，这条要提前和用户确认）。

**原取证事实（保留备查）**：
- MinerU 收表格任务时**吸收**表内图片 → `<img src="data:image/jpeg;base64,...">`、
  表内行内公式 → `<eq>...</eq>`，且被吸收对象从 model_list 删除避免二次输出
  （`tables.py:714-715`）；本仓表输出为纯文本 HTML。**注**：这条是 flash/hybrid 通路的
  读法，basic（无 VLM）是否走同一吸收逻辑**未证**，见上"表内图没量到"。
- 公式标签三分：`inline_formula` / `display_formula`（`formulas.py:112`）+
  `formula_number`（layout 标签 → `RAW_FORMULA_NUMBER`，surface 清单 §4.1），
  medium 模式下公式编号由 OCR-rec 识别（`formulas.py:182-234`）、hybrid 模式有
  专门的编号合并函数 `optimize_hybrid_formula_number_blocks`
  （`formulas.py:75-102`）。**第 0 步实测：这三类 label 在我们 dump 里都已经在**，
  所以本票不需要接新模型，只需要消费它们。
- 本仓 `mineru-basic` 挂 `with_formula_recognition`（`src/ocr_engine.rs:427-434`，条件挂载），
  公式件为**可选**（缺件只丢 LaTeX，见 19f6bf8 的参数面收敛票）。

**测试件本身的两条教训**（写进 `gen_formula_mixed.py` 头部注释，防后人重踩）：
1. 手写 x 偏移会让文本块互相重叠（第一版公式压住了 `relationn` / `connects` 的首字母），
   det/rec 读出残字 → 量到的"掉行"混了排版伪影。现在用 renderer **实测宽度**顺排。
2. `fig.add_axes` 是**归一化 + 左下角原点**，直接拿数据坐标算 y 会把格内图画到表格外
   （第一版画到尾行文字上，两家各吞半行）。现在用 `cell_band()` 显式换算。

---

## #10 块类型覆盖：chart / code / index / aside / footnote / list

状态：**未开始**。

**问题 → 动作**：code/index/aside/footnote 退化或丢失 → 按 basic 的 13 项（非 23 项）排顺序，且多数子项要等 #6。

**先收紧对比基线（本条目的口径修正）**：那 23 标签映射表叫
`VLM_LAYOUT_LABEL_MAP`（`constants.py:73`），唯一消费者是
`_build_vl_style_layout_blocks` → `_layout_item_to_content_block`
（`backend/analysis/pdf/layout.py:17,29,94`，`window.py:403` 调用），产物是**喂给
VLM 的 ContentBlock**——属 standard/advanced 线。与 MinerU **basic（=medium
effort）**真正对标的类型集合是 `PIPELINE_DET_TYPE`，**13 项**
（`constants.py:98-112`：TEXT / CODE / ASIDE_TEXT / INDEX / DOC_TITLE / RAW_CAPTION
/ FOOTER / PAGE_FOOTNOTE / HEADER / PAGE_NUMBER / PARAGRAPH_TITLE / REF_TEXT /
RAW_FOOTNOTE），证据是 `ocr.py:50-51`（`if effort == "medium": return
PIPELINE_DET_TYPE, True`）。→ 下表按 **13 项**算缺口，23 项是"将来接 VLM 时"的上限，
不要把两者混为一张债表。

| MinerU 类型 | 本仓现状 |
|---|---|
| `CODE` / `algorithm`（`constants.py:75`，且在 PIPELINE_DET_TYPE 内） | 无 fence，代码块退化为普通行 |
| `INDEX`（content 目录块） | 无缩进/点线还原 |
| `ASIDE_TEXT` | 与正文混排，页边注落进正文流 |
| `PAGE_FOOTNOTE` / `REF_TEXT` / `vision_footnote` | 无脚注/引用挂接 |
| `LIST` / `text_list` / `reference_list` | 只有**孤立前缀识别**（`src/reading_order/list.rs`，且刻意不做数字式避免与标题冲突），不成结构。**注意**：LIST 不在 `PIPELINE_DET_TYPE` 的 13 项里，列表结构是 VLM 线产物 → 优先级低于上面几行 |
| `CHART` | 仅存在于 `VLM_LAYOUT_LABEL_MAP`（`constants.py:77`）与 `LOCAL_LAYOUT_IMAGE_BLOCK_BODY_TYPES`（`constants.py:65`），**不在 basic 的 13 项内** → 属 VLM 线，非本轮债 |
| `DOC_TITLE` vs `PARAGRAPH_TITLE` | 级别由规则三信号投票给（`src/heading_levels.rs`），非 MinerU 的 LLM 分级（那条默认关闭，`config.py:395-397`）→ 口径差异需在 README 说明 |
| header / footer / page_number | 现按**噪声丢弃**（`src/reading_order/blocks.rs:15-22` 的 `NOISE_TYPES`，含 Seal），MinerU 走 `NOT_EXTRACT_TYPES` 不进提取、但在 content_list v2 有独立类型（`PAGE_HEADER`/`PAGE_FOOTER`/`PAGE_NUMBER`）→ 语义差别很大：我们**丢**，它**分流保留** |

**依赖**：本条目除"页眉页脚保留"外，多数子项依赖 #6（没有块级 IR 就无处安放）。
建议 #6 落地后按 chart → code → list 结构 → index/aside/footnote 顺序拆小票。
**例外**：页眉页脚"不丢弃而是标注 + 可选输出"可先做，因为它就是当前默认路径上的
一处信息损失，改动面小（一个 kind + 渲染器多一条分支）。**已落地（2026-09-28，
随 #6 第 5 步枚举先行一并交付）**：`RegionKind::Noise(NoiseKind)`/`Footnote`
producer 接线 + `ANYDOC_EMIT_FURNITURE` 可选输出，详见 #6 "第 5 步已落地"小节；
本条目其余子项（chart → code → list 结构 → index/aside/footnote 的类型识别与
渲染分支）仍待样本。

**验收判据**：每类各有 1 个合成/真实样本，输出结构可断言（列表是真列表、代码有
fence、旁注不进正文流）；未涉及类型的样本输出逐字节不变。

---

## #10b 印章 OCR 默认可用性（本轮新发现，独立小票）

状态：**已完成**（2026-09-27，按"行为变更须显式走 golden 重基线 + README 记一笔"
执行，不当 bugfix 悄悄开）。

**决定的来龙去脉**：`seal_pass` 在页面无 `Seal` 版面元素时**早退、一个模型都不加载**，
所以"默认开"的成本只落在真含章的页上（多 4.8MB 检测模型 + 每章一次 det+rec），
不含章的文档零影响——这是当初"要不要开"这个问题能答"开"的技术前提。另一侧的
代价是普通安装（无 `ANYDOC_MODEL_DIR`）遇到含章文档会**多一次 4.8MB 下载尝试**，
失败只告警一次并跳过、主链路结果原样返回，故离线 tiny/small 包不会因此变红。

**关闭入口的命名（唯一一处设计决断）**：新开 `ANYDOC_NO_SEAL_OCR`（存在即关闭、
不限值，与 `ANYDOC_NO_HYBRID` 同族语义），**不复用** `ANYDOC_SEAL_OCR` 翻转其含义。
理由：老变量历史语义是"存在即开启"，若把同名变量翻转成"存在即关闭"，老脚本里的
`ANYDOC_SEAL_OCR=1` 会**静默变成关闭印章**——那是最坏的漂移（用户看不出输出少了一行）。
老变量保留为**恒等于默认值的 no-op 别名**（设不设都是开），两个同时给出时以关闭为准
并 stderr 提示一次。开关内核 `seal_on_from(off, legacy)` 是纯函数，真值表已单测钉死。

**取证事实（保留原文，供后续复核）**：

取证：MinerU 在 **basic（medium）档默认就跑印章 OCR**——
`if effort in {"medium", "high"}: _apply_seal_ocr(...)`
（`backend/analysis/pdf/window.py:529-531`，实现 `ocr.py:266-320`），且
`seal_det` 是 basic 必需件之一（`registry.py:42-64`）。本仓此前 `ANYDOC_SEAL_OCR`
**默认关闭**，且 `Seal` 在我们这里进 `NOISE_TYPES` 被丢（`blocks.rs:21`）。

**#5a 的结论继续成立**：环排弧行仍显式跳过，本轮**没有**顺手解弧行。

**资产来路（已核，实施时用的就是这条）**：`seal_det` 在本仓注册表里有等价件
`pp-ocrv4_mobile_seal_det.onnx`（`registry.rs:70`，4,826,518 B）与
`pp-ocrv4_server_seal_det.onnx`（`:74`）；`seal_model_candidates`
（`src/ocr_post.rs`）的"本地名 → 注册表名 → 裸名下载"三级回退原样生效，故本轮
**没有**新增任何资产依赖。

**验收/回归**：`tests/ocr_post.rs` 的默认断言整体**反向**重写——基线运行（不带任何
开关）现在**必须**含 `【印章】专用章`，关闭态由 `ANYDOC_NO_SEAL_OCR` 给出；
"除该行外逐字节一致"的字节契约不变；新增 `legacy_seal_var_is_a_no_op_alias` 守别名
与"NO_ 赢"两条。`EngineKey` 早已含 seal 开关位，缓存不会串会话。

**待观察**：真实含章语料上的召回率（直排行）与 `【印章】` 行位置是否符合公文阅读
习惯，需要有现网样本后单独评。若某环境确认不需要印章，`ANYDOC_NO_SEAL_OCR=1` 是
唯一退回路径。

---

**依赖**：本条目除"页眉页脚保留"外，多数子项依赖 #6（没有块级 IR 就无处安放）。
建议 #6 落地后按 chart → code → list 结构 → index/aside/footnote 顺序拆小票。
**例外**：页眉页脚"不丢弃而是标注 + 可选输出"可先做，因为它就是当前默认路径上的
一处信息损失，改动面小（一个 kind + 渲染器多一条分支）。**已落地（2026-09-28，
随 #6 第 5 步枚举先行一并交付）**：`RegionKind::Noise(NoiseKind)`/`Footnote`
producer 接线 + `ANYDOC_EMIT_FURNITURE` 可选输出，详见 #6 "第 5 步已落地"小节；
本条目其余子项（chart → code → list 结构 → index/aside/footnote 的类型识别与
渲染分支）仍待样本。

**验收判据**：每类各有 1 个合成/真实样本，输出结构可断言（列表是真列表、代码有
fence、旁注不进正文流）；未涉及类型的样本输出逐字节不变。

---

## #11 结构化输出投影：content_list v2 优先，middle_json 次之

状态：**未开始，硬依赖 #6**。

**问题 → 动作**：没有结构化产物 → #6 完成后抄类型名/bbox 约定做常量表，再写 renderer（顺序不可颠倒）。

MinerU 输出面（`parser/api_server.py:146-154`）：`markdown` / `middle_json` /
`structured_content` / `html` / `latex` / `docx` / `zip`，其中 html/latex/docx
**需要有效 API key**（`OutputFormatReqToken`，`api_server.py:157`）。渲染器共 9 个
（surface 清单 §3.2）。本仓只有 markdown。

**取舍建议（按性价比排序）**：
1. **content_list v2 先做**——它是下游消费最广的形状，且我们不需要复刻 VLM 也能
   产出结构正确的版本（它只是 IR 的投影）；字段约定已知：按页数组，每 item
   `{type, content, bbox}`，**bbox 为 0–1000 归一化整数**
   （`render/_internal/content_list/common.py:118-122`）；类型名取自
   ContentTypeV2 24 项（`types.py:197-223`）。**不要自造类型名**，逐字抄该清单。
2. **middle_json 后做或不做**——严格版依赖 docvortex schema（见 #6 理由 3），
   legacy 版官方计划删除（见 #6 理由 2）。务实路径：先只做**读取兼容**（吃外部
   喂来的 content_list/middle_json 做校验），写出一版等 #6 稳定后再评。
3. `zip` 产物（markdown + images/ + json）在 #12 图片输出落地后几乎是免费的。

**验收判据**：同一文档，本仓 content_list v2 与 MinerU basic 的 content_list v2 在
**类型序列**与块数上对齐率可量化（复用已有 IoU 对拍工具链，README 的
`ANYDOC_DUMP_DIR` 一行）；schema 字段名与本仓单测里硬编码的 MinerU 常量表逐字等值。

---

## #12 裸图片文件输入

状态：**已完成**（2026-09-27，本节成本最低的一条按计划先做）。

**实现落点**：
- `src/detect.rs`：新增 `DocKind::Image`，判定按**魔数 + 扩展名**双路。魔数
  （`looks_like_raster`）不是可有可无的加分项——`-` 入口落地的 NamedTempFile
  **没有扩展名**，纯扩展名分流会把扫描图丢给 anydoc 兜底并给出"格式不支持"的
  误导结论；扩展名分支则保证内容损坏的图片（截断 JPEG）仍归 `Image` 并报"解码
  失败"。`is_known_extension` 同步收录 8 种，批目录才收得到图。
- `src/convert.rs`：`load_image_for_ocr` + `convert_image`。像素闸挂在
  `decoder.dimensions()`（**解码前**），2 万像素的图不会先分配位图再报错；
  超限走 `ResourceLimit` 显式拒绝，**不静默降采样**（与 PDF 渲染通路的自动降
  scale 分工不同：那是我方选的渲染参数可退，这是用户原图的像素）。EXIF 方向
  在送 OCR 前 `apply_orientation` 转正——mineru 档 `doc_ori` 按 MinerU 口径是
  关闭的（#8 注释），不转正等于把整页侧着喂检测模型。
- OCR 复用 `ocr_engine::ocr_images`（等同"单页图片型 PDF"），后处理链含印章
  pass（`OcrEngine::build` 统一咨询 `PostPass::wanted()`），不另开一条通路。

**已定语义（原"要先定"的那几件）**：gif/tiff **只取首帧**，已在 `--help`
（`models::MINERU_ENGINE_HELP` 新增"输入格式"段）与 README 注明，不留多页悬念；
`jp2` 与 MinerU 的扩展名/魔数对齐但 `image` 0.25.10 **无 JPEG2000 解码器** →
显式 `unsupported`（比静默走 anydoc 兜底诚实）；`--pages` 对图片按既有非 PDF
口径拒绝。

**验收**：`tests/image_input.rs` 覆盖魔数/扩展名分流、无扩展名 stdin 形状、像素闸
在 OCR 之前开火、`--pages` 拒绝、jp2 诚实报错；`src/detect.rs` 新增魔数表/BMP
保留位护栏单测。全部**不需要模型**即可跑（CI 绿得住）。

---

## #13 解析模式无显式 `txt` 侧开关

状态：**已完成**（2026-09-27，定名 `--text-only`，只留一套）。

**定名**：`--text-only`。没用 `--ocr-mode txt` 是因为本仓没有 `--ocr-mode` 这个
枚举面，硬造一个枚举开关与既有 `--pdf-force-ocr`/`--ofd-force-ocr` 两个 bool 混在
一起会立刻产生"两个真相互相矛盾"的组合（`--ocr-mode txt` + `--pdf-force-ocr`？）；
也没用 `--pdf-text-only`，因为该开关对 OFD **同样**成立，名字里带 pdf 是误导。
只留这一个名字，CLI 与库 `ForceFlags::text_only` 一一对应。

**语义（比 MinerU 更严，这点必须在文档里说清）**：MinerU 的 `txt` 在 medium 档仍会
为图片块加载版面/OCR，本开关**承诺零模型加载**——三处加载入口全部堵死：
1. `src/pdf/text_layer.rs::confirm_table_pages`（文字层探针里**唯一**会建引擎的动作）
   在 `text_only` 下直接返回空表——否则一张"疑似表页"就把整套 mineru 模型拉起来；
2. `src/ofd/mod.rs::convert_ofd` 跳过 `init_runtime` 与两个 OCR pass；
3. `src/convert.rs::convert_image` 图片输入无文字层可抽，直接 `NeedsOcr` 拒绝。

**行为矩阵**：图片型 PDF / 全 OCR 的 OFD → 显式 `NeedsOcr`（不静默出空文档）；
混合 PDF / 部分页缺字的 OFD → 按文字层输出 + stderr **列出缺页号**（静默丢页正是
当初 hybrid 路由要修掉的缺陷，这里不算"悄悄降级"）；与两个 force 开关互斥 →
`Unsupported`（CLI 早拒 + 库侧各通道同名闸，库调用方绕不过）。

**验收**：`tests/image_input.rs` 的 `text_only_works_with_an_empty_model_home` 就是
"空 `$OAR_HOME` + 无 `ANYDOC_MODEL_DIR`"那条实测（子进程 env 显式覆盖成临时空目录，
断言成功出内容且 stderr 无任何下载字样）；另有扫描件 `needsOcr`、混合文档告警列页号、
互斥校验先于 IO 三条。`--help` 里写明了与 MinerU `txt` 的差别，避免按 MinerU 口径
理解本开关。

---

## #14 VLM / LLM 辅助线：判定为范围外（除非以客户端方式接）

状态：**已完成（文档口径已落地，2026-09-27）**。本条目本来就是决策条目、非实现任务；
唯一要做的实现是"附带必做"那两条文档，现已落地：README「模型档与精度」新增一段
写明 basic ≠ MinerU 默认档 standard（含 VLM）、别拿 standard 结果当基线，并列出
`title_leveling` / `cross_page_table_cell_merge` 默认全关这一处口径差异；`--help`
（`MINERU_ENGINE_HELP`）新增「对齐口径」小节，内容与 README 同源并指回本节。

**问题 → 动作**：默认档口径不可比 → 只写文档；真要做就是 `--server-url` 客户端，不做本地推理。

**范围外**（与"CPU / 离线 / 单文件分发"的仓定位正面冲突）：
standard/advanced 两档、`MinerU2.5-Pro-2605-1.2B` VLM 权重、4 个引擎
（llama-cpp / vllm / lmdeploy / mlx，`model/vlm/selector.py:11-54`）、
VLM 负责的多栏阅读顺序与图表内容分析；以及 FastAPI v1 API（分片上传 + sha256
去重 + 任务队列）、doclib（SQLite + FTS + worker）、Gradio WebUI、Router、
telemetry、agent 向 `read`/`search`/`--limit`/`--after` 游标。

**LLM 辅助后处理**（`title_leveling`、`cross_page_table_cell_merge`）：
MinerU **默认全关**（`config.py:395-397`），所以"不实现"就是与默认行为对齐，
不算缺口；本仓对应实现走的是规则路径（`heading_levels.rs` 三信号投票 /
`cross_page_table.rs` 几何列数对齐），语义上比它的默认更确定 → **维持现状**。
这一处口径差异已写进 README（见本节首的状态行）。

**如果要接，务实路径只有一条**：不做本地推理，当**客户端**。MinerU 的 v1 API 协议
和 OpenAI 兼容形态已在 `parser/api_client.py`（上传→提交→轮询→下载，
且兼容 3.x middle_json，`api_client.py:1169-1175`）里现成，本仓加
`--server-url` 一个参数即可把 standard/advanced 变成可选后端，不污染主链路、
不引入权重。开工条件：有人真的需要 standard 档精度，且接受网络依赖。

**附带必做（已完成）**：README 与 `--help` 明确
"本仓对齐 MinerU **basic** 档（无 VLM）；MinerU 默认档为 standard（含 VLM）"，
并链接本节口径。→ 2026-09-27 已落地，位置：README「模型档与精度」第二段、
`src/models.rs` 的 `MINERU_ENGINE_HELP`「对齐口径」小节、README `--ocr-tier` 行注。

---

## #1 表格/版面窗口化推理

状态：**判定为无需实现**（口径对齐，非缺陷）。

MinerU 的 `window` 相关逻辑服务于"整页图 + 版面框"的分块喂送；本仓走 oar-ocr
的整页推理 + `predict_parallel` 页级并发，且 `--pages`（a25f35a）已提供文档级窗口。
单页内窗口化对本仓的模型规格（tiny/small 整页输入尺寸固定）不产生精度或显存收益，
故不实现。若后续引入大页（A0/长图）再评估。
