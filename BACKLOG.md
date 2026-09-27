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
| #6 | 只有 markdown 一种输出；想加任何结构化输出都得改主链路 | 用 `ANYDOC_DUMP_DIR` 对照 MinerU item 字段，**列** IR 缺失字段清单（块边界/标题级别/span/页尺寸），不改代码 | 块级类型+级别+bbox+span 可单测断言，且 markdown 对 6 样本 hash 全等（**未**用 UPDATE） | 阻塞 #10/#11 · 大 |
| #7 | 无线表/合并单元格表的结构识别**怀疑**不如 MinerU basic | 拿同一张无线表跑「现状」vs「`rt-detr-l_wireless_table_cell_det.onnx` cells→HTML（已在注册表）」，比结构正确率；并确认 UNet 能否进 `with_wireless_table_structure` | 无线表与 MinerU basic 的表格 HTML 结构不一致数**下降**；有线表逐字节不变 | **第 0 步已完成、两条路都不进默认**（(ii) 判死，(i) 已实现为默认关闭的 `ANYDOC_WIRELESS_CELLS`）· 剩余部分待现网语料 |
| #10b | 含章公文页默认不出印章文字，而 MinerU basic 默认出 | 决定"默认开"能否接受为行为变更（普通安装多 4.8MB 下载 + 新增 `【印章】` 行） | seal_scan 默认档出直排行文字 + golden 重基线 + README 记一笔 | **已完成** · 小 |
| #12 | `anydoc scan.png` 进不了 OCR 管线（MinerU 直接吃 8 种图片） | `detect` 加 `DocKind::Image`，复用整页 OCR；像素闸要在**加载**处补算一次 | png 出 markdown；>3500px 显式 `resourceLimit`；gif/tiff 只取首帧且 `--help` 注明 | **已完成** · 小（本节最便宜） |
| #13 | 无法强制"只用文字层、绝不联网下载模型" | 定名：`--pdf-text-only` 还是 `--ocr-mode txt`（**只留一套**） | 空 `$OAR_HOME` + 断网实测不加载模型 | **已完成**（定名 `--text-only`）· 小 |
| #8 | 图片型扫描件里旋转的表格没有第二道方向信号（表被转置/正文被吞） | 先查 `with_table_orientation`（`structure.rs:442`）要吃哪个模型、是否在我们注册表 | 合成旋转表在 `--pdf-force-ocr` 下网格正确；非旋转页逐字节不变 | 排在 #7 后（同通路，防冲突）· 小-中 |
| #9 | 表格里的公式/图片丢；行内公式是否已对齐**未知** | 拿含行内公式与公式编号的样本 `ANYDOC_DUMP_DIR` 对拍，先量"已具备/缺失"再决定做多少 | 行内公式 `$...$` 不掉行；编号不重复成独立行；表内对象有去处 | 公式件需 `ANYDOC_MODEL_DIR` · 中 |
| #10 | code 无 fence、目录无缩进、旁注混进正文、脚注/引用不挂接 | 按 `PIPELINE_DET_TYPE` 13 项内顺序 CODE → INDEX → ASIDE_TEXT → FOOTNOTE/REF_TEXT，各配 1 个样本 | 每类输出结构可断言；未涉及类型逐字节不变 | 除页眉页脚外全依赖 #6 · 中-大 |
| #11 | 没有可被下游消费的产物（content_list / middle_json） | #6 完成后，先把 24 个类型名与 bbox 约定抄成**单测常量表**，再写 renderer | 与 MinerU basic 同文档的类型序列/块数对齐率可量化；markdown 输出不变 | #6 · 中 |
| #14 | 与 MinerU 默认档（standard，含 VLM）精度不可比 | 只写文档口径；如要接，只做 `--server-url` 客户端，不搬权重 | README/`--help` 口径落地即结 | 无 · 文档级 |

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

- `RegionKind::Body` 的 `text` 已由 producer 完成"阅读顺序还原 + **标题前缀注入**"
  （`src/region.rs:26-28` 注释明写"`text` 即最终行"）；
- `RegionKind::PreRendered` 存的是"成品 markdown 片段（含精确分隔符），渲染层原样
  追加、不二次加工"（`src/region.rs:31-34`）；
- 块级语义只剩 4 个 kind（Body/Grid/TableHtml/PreRendered），**没有**：标题级别、
  header/footer/page_number 的区分（现被当噪声丢弃，见 `src/gfm_adapter.rs:5-6`）、
  span（行内粗斜体/行内公式）、图片资产引用、页尺寸（只在 `ANYDOC_DUMP_DIR` 的旁路
  JSON 里，`src/pdf/mod.rs:299-301`）。

→ 从当前 IR **无法**还原出块边界与级别，字符串已经焊死了。因此 #6 的第一件事是
把结构信息从 producer 的输出字符串里**解耦**：producer 产"块 + 级别 + span"，
`#` 前缀与分隔符下移到渲染器。

### 风险（务必先读）

解耦标题前缀会动到**现网字节一致契约**（golden 9 样本 / batch 6 样本守护）。
缓解：判定视图与渲染视图分离——本仓在 `ANYDOC_RICH_TEXT` 上已有"前缀与样式共存、
在剥标记后的判定视图上跑启发式"的成熟做法（README 环境变量表），照搬同一手法；
新增 `--format markdown` 之外的输出时**不动** markdown 渲染器的任何字面输出，
用"同一 IR 两个 renderer"来证明等价（新 renderer 的 markdown 输出与旧通路
逐字节等值，是 #6 的硬验收）。

### 验收判据

- IR 层面：块级类型 + 标题级别 + bbox + span 可被单测直接断言（当前不可）；
- 零回归：`cargo test --release --test golden`（`ANYDOC_GOLDEN_OCR=1`）在解耦后
  仍 "9 checked"，且**未使用** `ANYDOC_GOLDEN_UPDATE`；
- 等价证明：markdown renderer 改造前后对 6 个 batch 样本 hash 全等；
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

状态：**未开始**。

**问题 → 动作**：扫描件里旋转的表没有视觉兜底 → 先查所需模型是否在我们的注册表里，再决定是否接 `with_table_orientation`。

MinerU 表格三阶段第一步是"先 PDF 原生文本行投票，**不足时**调视觉方向分类模型"
（`mineru/backend/analysis/pdf/tables.py:530-559`，模型
`table/cls/mineru_table_ori_cls.py`）。本仓 `src/orientation.rs` 实现了同一套
PDF 线投票口径（046b08a ②），但**没有视觉兜底** → 纯图片型扫描件里旋转 90°/180°
的表格没有第二道信号。附带：本仓 `mineru-basic` 档按 MinerU 口径**关闭**页面方向
矫正（`OcrTier::MineruBasic` 的 `doc_ori: ""`，`src/models.rs:251`；口径注释见 `src/ocr_engine.rs:402-404`），这是刻意的对齐，不要顺手打开。

**验收判据**：合成样本（表块旋转 90°）在 `--pdf-force-ocr` 下网格重建正确；
非旋转页面输出逐字节不变；上游 `with_table_orientation`（`structure.rs:442`）
需要哪个模型资产要先查（本仓注册表里有 `pp-lcnet_x1_0_table_cls.onnx`，
方向分类件是否在册未确认）。

---

## #9 表内对象吸收 + 行内公式 / 公式编号

状态：**未开始**。

**问题 → 动作**：表内公式/图丢，行内公式覆盖度**未实测** → 先对拍量差距，不许凭代码痕迹开工。

**取证事实**：
- MinerU 收表格任务时**吸收**表内图片 → `<img src="data:image/jpeg;base64,...">`、
  表内行内公式 → `<eq>...</eq>`，且被吸收对象从 model_list 删除避免二次输出
  （`tables.py:714-715`）；本仓表输出为纯文本 HTML，表内图/公式直接丢。
- 公式标签三分：`inline_formula` / `display_formula`（`formulas.py:112`）+
  `formula_number`（layout 标签 → `RAW_FORMULA_NUMBER`，surface 清单 §4.1），
  medium 模式下公式编号由 OCR-rec 识别（`formulas.py:182-234`）、hybrid 模式有
  专门的编号合并函数 `optimize_hybrid_formula_number_blocks`
  （`formulas.py:75-102`）。
- 本仓 `mineru-basic` 挂 `with_formula_recognition`（`src/ocr_engine.rs:427-434`，条件挂载），
  公式件为**可选**（缺件只丢 LaTeX，见 19f6bf8 的参数面收敛票），但**行内公式覆盖到
  什么程度未实测**：上游 `structure.rs:2816-2824` 有"inline formula 的 sort_and_join +
  `label="formula"` 包裹"的痕迹，说明通路存在，不等于我们对齐。

**第 0 步**：拿一张含行内公式与公式编号的样本跑 `ANYDOC_DUMP_DIR`，与本仓 markdown
对拍，先量出"已具备/缺失"，再决定做多少——不要凭代码痕迹开工。

**验收判据**：行内公式在正文行内以 `$...$` 就位（不掉行、不吞前后文）；公式编号
不重复成独立行；表内公式/图片有明确去处（吸收或至少在正文里不丢）。

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
一处信息损失，改动面小（一个 kind + 渲染器多一条分支）。

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
一处信息损失，改动面小（一个 kind + 渲染器多一条分支）。

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

状态：**决策条目，非实现任务**。写在这里是为了下次不必重做权衡。

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
唯一要做的是在 README 写清这一处口径差异（与 #10 表格末行同源）。

**如果要接，务实路径只有一条**：不做本地推理，当**客户端**。MinerU 的 v1 API 协议
和 OpenAI 兼容形态已在 `parser/api_client.py`（上传→提交→轮询→下载，
且兼容 3.x middle_json，`api_client.py:1169-1175`）里现成，本仓加
`--server-url` 一个参数即可把 standard/advanced 变成可选后端，不污染主链路、
不引入权重。开工条件：有人真的需要 standard 档精度，且接受网络依赖。

**附带必做**（不需要写代码之外的事）：README 与 `--help` 明确
"本仓对齐 MinerU **basic** 档（无 VLM）；MinerU 默认档为 standard（含 VLM）"，
并链接本节口径。

---

## #1 表格/版面窗口化推理

状态：**判定为无需实现**（口径对齐，非缺陷）。

MinerU 的 `window` 相关逻辑服务于"整页图 + 版面框"的分块喂送；本仓走 oar-ocr
的整页推理 + `predict_parallel` 页级并发，且 `--pages`（a25f35a）已提供文档级窗口。
单页内窗口化对本仓的模型规格（tiny/small 整页输入尺寸固定）不产生精度或显存收益，
故不实现。若后续引入大页（A0/长图）再评估。
