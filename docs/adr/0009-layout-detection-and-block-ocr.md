# ADR-0009：流程对齐 MinerU（块驱动 + 模型阅读序 + 选择性噪声过滤）

- 状态：拟议（已质询 sharpened）
- 日期：2026-08-16（细化）
- 决策者：anydoc-ocr 维护者
- 上下文：ADR-0008（图像直提）落地后，GJB 9001C 对比测试暴露准确率差距——anydoc-ocr 77%，MinerU 99.6%。初版 ADR-0009 误判"我们无版面检测"，二版核查后发现 oar_ocr 已提供完整版面能力但被主动绕过。本版（三版）经 deepwiki + MinerU 源码核查 + grill-with-docs 质询，**从"打补丁式过滤"升级为"流程对齐 MinerU 的块驱动通路"**。

## 背景

### 现状核查（关键修正，三版）

[ocr_engine.rs](file:///workspace/src/ocr_engine.rs#L189-L214) 的 `build_analyzer` 已配置 PicoDet-Layout 版面模型；[StructureResult](https://docs.rs/oar-ocr/0.9.1/oar_ocr/domain/structure/struct.StructureResult.html) 返回完整版面数据。

**三版新发现（docs.rs 核查 oar-ocr 0.9.1 实际字段）**：`LayoutElement` **已自带 `order_index: Option<u32>`**（模型给出的阅读序索引，**等价于 MinerU `layout_dets[*].index`**），以及 `seg_start_x/seg_end_x`（列分段边界）、`num_lines`（块内行数）。`RegionBlock::element_indices` 索引的是 `layout_elements`（**不是 text_regions**），是更高层的列/节分组。

这意味着：**模型早已给出阅读序，我们一直没用**——`reading_order.rs` 的"最大间隙列检测"启发式是在重复造模型已经提供的轮子。

[gfm_adapter.rs](file:///workspace/src/gfm_adapter.rs#L1-L9) 现状：主动绕过版面，把全页 `text_regions` 喂给自己写的列检测 + y 排序。这是水印/页眉混入正文、目录点线拆碎的根因。

### MinerU 流程对照（pipeline_magic_model.py 源码核查）

经 [MinerU pipeline_magic_model.py](https://github.com/opendatalab/MinerU/blob/master/mineru/backend/pipeline/pipeline_magic_model.py) 源码核查，MinerU pipeline 后端的实际流程：

```
layout_dets（模型输出，自带 index 阅读序）
  → PP_DOCLAYOUT_V2_LABELS_TO_BLOCK_TYPES 映射（footer/header/number → 噪声 BlockType）
  → __fix_axis()（bbox 修正，删除 w/h<=0 的 span）
  → __post_process()（index 重排，填充行内公式和文本 span）
  → 按 block 类型分派：TEXT→OCR spans / TABLE→表格识别 / IMAGE→图片 / 噪声→丢弃
  → 每个 block 内：merge_spans_to_line → line_sort_spans_by_left_to_right → _merge_para_text
  → 块间按 index 输出
```

**关键差异**：MinerU 是**块驱动**（iterate blocks in model order, dispatch by type, merge spans within block），我们是**区域驱动**（take all text_regions, run our own column detection）。这是流程层面的根本差距，不是"加个过滤器"能补的。

| 环节 | MinerU（源码核查） | anydoc-ocr 现状 | 差距影响 |
|------|------|----------------|---------|
| 阅读序 | `layout_dets[*].index`（模型给出） | 自写最大间隙列检测 | 目录点线拆碎、双列交错 |
| 流程骨架 | 块驱动：iterate block → 分派 → 块内合并 | 区域驱动：全页 regions → 列检测 → y 排序 | 水印当正文、跨块混排 |
| 噪声过滤 | 按 BlockType 直接丢弃 header/footer/number | 完全未用 | 插入错误 3330 |
| 段落合并 | `merge_spans_to_line` + `_merge_para_text` | 每 region 一行，不合并 | 目录每行一段（222 段 vs GT 177） |
| 置信度阈值 | **源码未见阈值过滤**，按类型全量分派 | — | 见质询 Q5 |

## 质询记录（grill-with-docs）

### Q1：D1 是"对齐 MinerU"还是"在区域驱动上加补丁"？

**质询**：二版 D1 提议"过滤噪声 region，其余 pipeline 不变"。但 MinerU 是块驱动，我们是区域驱动。在区域驱动上加过滤器，骨架没变，**不算流程对齐**。用户明确要求"流程和 mineru 对齐"。

**决策**：**改为块驱动**。新流程：
1. 按 `LayoutElement::order_index` 排序版面块（对齐 MinerU `index`）
2. 跳过噪声类型块（Header/Footer/Number/Seal）——对齐 MinerU 按 BlockType 丢弃
3. 对每个保留块，收集中心点落在其 bbox 内的 `text_regions`，块内按 y 排序合并为段落
4. 块间按 order_index 输出

**与二版的区别**：二版是"过滤后仍跑全页列检测"；三版是"按块迭代，块内自排序，不再跑全页列检测"。列检测由模型的 `order_index` + `RegionBlock` 接管。

### Q2：order_index 为 None 怎么办？

**质询**：`LayoutElement::order_index` 是 `Option<u32>`。旧模型/简单页可能不输出。MinerU 的 `layout_dets[*].index` 似乎总是存在（源码里直接 `layout_det['index']` 取值）。

**决策**：**三级降级链**：
1. 首选 `LayoutElement::order_index`（模型阅读序，对齐 MinerU）
2. 次选 `RegionBlock::order_index` + `element_indices`（PP-DocBlockLayout 列级分组，处理双列：左列 region 的 elements 全部先于右列）
3. 降级：现有 `reading_order::order_text_regions`（最大间隙启发式，保留不动）

只有 (1)(2) 都 None 时才降级到 (3)。降级是兜底，不是主路径。

### Q3：段落合并（merge_spans_to_line）做不做？

**质询**：MinerU 在每个 block 内做 `merge_spans_to_line` + `_merge_para_text`，把多个 span 合并成段落。我们现状是每个 `text_region` 输出一行，**这是目录 222 段 vs GT 177 段的主因**——目录每行点线是一个 text_region。不做段落合并，阅读序再准也拆碎。

**决策**：**做，但范围限定**。块内 text_regions 按 y 排序后，相邻 region 满足以下条件之一则合并为同一段落：
- y 间距 < 块内行高的 1.5 倍（启发式行间距）
- 同一 `LayoutElement` 的 `num_lines` 已给出块内行数，合并到该行数

**不做** MinerU 的 `line_sort_spans_by_left_to_right`（块内 LTR 行排序）——我们的 text_region 已是行级，无需再切。这是"对齐流程"而非"逐行复刻"。

### Q4：30% 降级保护要不要？

**质询**：二版 D1 的"过滤后文本量 <30% 回退"有语义冲突——封面页/水印页本就该输出少。强制回退会重新引入噪声。MinerU 源码未见此保护。

**决策**：**去掉 30% 降级，改为"块类型白名单 + 置信度双门"**：
- 只过滤 `element_type ∈ {Header, HeaderImage, Footer, FooterImage, Number, Seal}` **且** `confidence ≥ 阈值` 的块（双门）
- 噪声块判定不再依赖"过滤后文本量"这种间接指标
- 真正的降级是 Q2 的 (3)：order_index 全 None 时回退区域驱动

**阈值来源**：见 Q5。

### Q5：置信度阈值 0.7 哪来的？

**质询**：二版写死 0.7，无来源。ADR-0007 的 Laplacian 阈值有"业界标准"背书，layout 置信度阈值有吗？MinerU 源码未见按 confidence 过滤 layout_dets——它按类型全量分派。

**决策**：**分两步**：
1. **首版不设置信度阈值**（对齐 MinerU：按类型过滤，不按分数过滤）。若版面把整页判 Header，那是模型问题，加阈值也救不了——只会误杀真 Header。
2. **观测后再定阈值**：跑测试集，统计噪声块的 confidence 分布。若真有"低置信度噪声块误伤正文"的 case，再引入 `ANYDOC_NOISE_CONF` 环境变量，默认 0.0（不过滤）。

**否决二版的 0.7 写死值**：无数据支撑，且与 MinerU 不一致。

### Q6：块驱动下，单 Text 块内双列怎么办？

**质询**：若版面模型把双列正文判成 1 个 Text 块，块内 y 排序会把左右列交错。MinerU 有 `is_vertical_text_block_by_spans` 和块内 LTR 排序，但那是行内 span 排序，不是列分离。

**决策**：**块内仍跑列检测**。复用 `reading_order::detect_column_split`，但作用域从"全页"收窄到"单块内的 text_regions"。若块内检出双列，块内按左列全→右列全排序；否则 y 排序。这样：
- 模型判对列（RegionBlock 给出双 region）→ 块间 order_index 已分离
- 模型判错列（单 Text 块裹双列）→ 块内列检测兜底

### Q7：CER <10% 是不是错目标？

**质询**：CER 把插入/删除/替换混在一起。水印过滤主要降插入，目录合并主要降删除（段数减少不降 CER，但降段数）。单一 CER 数字掩盖哪种错误主导。

**决策**：**分指标验收**：
- 插入率（Insertion / GT chars）：水印过滤有效性，目标 <2%（现状 ~15%）
- 替换率（Substitution / GT chars）：OCR 模型本身能力，不归本 ADR，目标不变
- 段落数：目录段数 222 → 接近 GT 177（±10%）
- CER 总值：<10% 作为综合参考线，但不作为唯一验收门

### Q8：影响面会不会波及文字层通路？

**质询**：`gfm_adapter.rs` 是图片型 PDF/OFD 的 OCR 通路。`pdf/text_layer.rs` 是文字层通路，也用了 `LayoutElementType::Table`。改 gfm_adapter 会不会波及文字层？

**决策**：**不波及**。三版改动全部限定在 `gfm_adapter.rs`（块驱动重构）+ `reading_order.rs`（新增块内列检测复用，不改现有 `order_text_regions` 签名）。`text_layer.rs` 的 `LayoutElementType::Table` 用法不动。golden 测试只需更新 OCR 通路 fixture（图片型 PDF），文字层 fixture 不变。

## 决策

### D1：块驱动通路（对齐 MinerU 流程骨架）

新增 `gfm_adapter::block_driven_order`，替代现有"全页 regions → 列检测 → y 排序"主路径：

```rust
/// 块驱动阅读序（对齐 MinerU：iterate blocks in model order, dispatch by type）。
///
/// 三级降级：order_index → RegionBlock → order_text_regions（区域驱动兜底）。
fn block_driven_order(page: &StructureResult, regions: &[Region]) -> Vec<String> {
    // 1. 收集有效块：跳过噪声类型（Header/Footer/Number/Seal），按 order_index 排序
    let noise = [Header, HeaderImage, Footer, FooterImage, Number, Seal];
    let mut blocks: Vec<&LayoutElement> = page.layout_elements.iter()
        .filter(|el| !noise.contains(&el.element_type))
        .collect();
    // 2. 排序：order_index 优先；None 的块按 bbox.y_min 兜底
    blocks.sort_by_key(|el| el.order_index.unwrap_or(u32::MAX));
    let has_order = blocks.iter().any(|el| el.order_index.is_some());
    if !has_order {
        // 全无 order_index → 降级到 RegionBlock 或区域驱动（Q2 链路）
        return fallback_order(page, regions);
    }
    // 3. 块内：收集中心点落在 bbox 内的 regions，块内列检测 + y 排序 + 段落合并
    let scale = page_scale(page);
    let mut out = Vec::new();
    for blk in blocks {
        let mut inner: Vec<Region> = regions.iter()
            .filter(|r| norm_membership(r.cx(), r.cy(), scale, &blk.bbox))
            .cloned()
            .collect();
        if inner.is_empty() { continue; }
        // 块内列检测（Q6）：单块裹双列时分离
        let lines = order_within_block(&inner);
        // 段落合并（Q3）：相邻行 y 间距 < 行高 1.5x 则合并
        out.extend(merge_into_paragraphs(lines));
    }
    out
}
```

### D2：三级降级链（Q2）

```rust
fn fallback_order(page: &StructureResult, regions: &[Region]) -> Vec<String> {
    // 次选：RegionBlock 分组（PP-DocBlockLayout 列级阅读序）
    if let Some(rbs) = &page.region_blocks {
        let mut sorted_rbs: Vec<&RegionBlock> = rbs.iter()
            .filter(|rb| rb.order_index.is_some())
            .sorted_by_key(|rb| rb.order_index.unwrap())
            .collect();
        if !sorted_rbs.is_empty() {
            // 每个 region 内的 elements 按 element_indices 取出，再块内排序
            // ...（同 D1 块内逻辑，但块集合来自 region.element_indices）
        }
    }
    // 末选：现有区域驱动（reading_order::order_text_regions，不动）
    order_text_regions(regions)
}
```

`reading_order::order_text_regions` 保留原签名，作为兜底。**不删不改**，保证降级路径与现状等价。

### D3：段落合并（Q3，限定范围）

```rust
/// 块内段落合并：相邻行 y 间距 < 行高 1.5x → 同段。
fn merge_into_paragraphs(lines: Vec<String>) -> Vec<String> {
    // 注：lines 已带 y 坐标信息（用 (y, text) 元组），此处简化
    // MinerU _merge_para_text 对齐：行间无空行则合并，行末无句末标点则合并
    // ...
}
```

**不做** MinerU 的 `line_sort_spans_by_left_to_right`（行内 span 排序）——text_region 已是行级。

### D4：噪声过滤改为类型白名单（Q4/Q5）

**首版不设置信度阈值**（对齐 MinerU 源码：按类型过滤，不按分数）。噪声类型集合固定：
```rust
const NOISE_TYPES: &[LayoutElementType] = &[
    Header, HeaderImage, Footer, FooterImage, Number, Seal,
];
```

观测后若需引入阈值，加 `ANYDOC_NOISE_CONF` 环境变量，默认 0.0（不过滤）。

**否决二版的"30% 降级保护"**：语义冲突（封面页本就该少），且 MinerU 无此机制。

### D5：不引入新模型（延续二版 D3）

oar_ocr 的 PicoDet-Layout + PP-DocBlockLayout 已提供：
- 28 类语义块（含噪声类型）— 对齐 MinerU `PP_DOCLAYOUT_V2_LABELS_TO_BLOCK_TYPES`
- `LayoutElement::order_index` — 对齐 MinerU `layout_dets[*].index`
- `RegionBlock` 列级分组 — 对齐 MinerU 双列处理

能力足够，问题是"有不用"。三版的核心就是把已有能力用起来。

## 实施计划

### 阶段 1：块驱动骨架（gfm_adapter.rs）

- [ ] 实现 `block_driven_order`：order_index 排序 + 噪声类型过滤 + 块内 regions 收集
- [ ] 实现 `fallback_order`：RegionBlock → order_text_regions 三级链
- [ ] **验收点**：单测覆盖 (a) 有 order_index 的页 (b) 全 None 降级 (c) 噪声块被跳过
- [ ] 不改 `order_text_regions` 签名（降级路径等价）

### 阶段 2：块内列检测 + 段落合并

- [ ] 实现 `order_within_block`：复用 `detect_column_split` 逻辑，作用域收窄到块内
- [ ] 实现 `merge_into_paragraphs`：相邻行 y 间距 < 行高 1.5x 合并
- [ ] **验收点**：目录段数 222 → <200（接近 GT 177 ±10%）

### 阶段 3：对比测试（分指标，Q7）

- [ ] sdoac_scan.pdf 重跑，分指标统计：
  - 插入率：现状 ~15% → 目标 <2%
  - 段落数：现状 222 → 目标 <200
  - CER 总值：现状 23.56% → 参考 <10%
- [ ] 耗时无增加（版面模型已在跑，仅利用其输出）
- [ ] golden 测试更新（仅 OCR 通路 fixture，文字层不动 — Q8）

### 阶段 4（可选，观测后决策）：置信度阈值

- [ ] 跑测试集，统计噪声块 confidence 分布
- [ ] 若有"低置信度噪声误伤正文"case，引入 `ANYDOC_NOISE_CONF`（默认 0.0）
- [ ] 若无，本阶段跳过

## 风险与缓解

| 风险 | 影响 | 缓解 |
|------|------|------|
| 块驱动骨架改动大，引入新 bug | 回归 | 三级降级链保证：order_index 全 None 时退化为现状（order_text_regions），行为等价 |
| 模型 order_index 错排（如双列判反） | 阅读序错乱 | RegionBlock 兜底（列级分组）；块内列检测兜底（Q6） |
| 段落合并误并（如标题与正文并） | 标题层级丢失 | 合并前先按 `is_title()` 块切分，标题块不参与合并 |
| 噪声块 bbox 不准，误伤邻近正文 | 正文丢失 | 首版无阈值，按类型过滤；bbox 收缩 10% 再判包含（沿用二版） |
| golden 大面积更新 | 测试基线漂移 | 仅 OCR 通路 fixture 受影响（Q8），文字层 fixture 不变；`ANYDOC_GOLDEN_UPDATE=1` 重生成 |

## 非目标

- 不引入 DocLayout-YOLO（oar_ocr 的 PicoDet-Layout 已够用，D5）
- 不做分块 OCR（块内仍用整页 OCR 的 text_regions，不绕过 oar_ocr 的 det/rec）
- 不做 MinerU 的 `line_sort_spans_by_left_to_right`（行内 span 排序，text_region 已行级）
- 不做 middle_json 结构化（P2 再考虑）
- 不做公式识别（MFD/MFR）
- 不做跨页段落续接（MinerU-Popo 范畴，远期）

## 引用

- [oar_ocr LayoutElement（含 order_index/seg_start_x/num_lines 字段）](https://docs.rs/oar-ocr/0.9.1/oar_ocr/domain/structure/struct.LayoutElement.html)
- [oar_ocr RegionBlock（PP-DocBlockLayout 列级分组，element_indices 索引 layout_elements）](https://docs.rs/oar-ocr/0.9.1/oar_ocr/domain/structure/struct.RegionBlock.html)
- [oar_ocr StructureResult](https://docs.rs/oar-ocr/0.9.1/oar_ocr/domain/structure/struct.StructureResult.html)
- [MinerU pipeline_magic_model.py 源码（PP_DOCLAYOUT_V2_LABELS_TO_BLOCK_TYPES 块类型映射）](https://github.com/opendatalab/MinerU/blob/master/mineru/backend/pipeline/pipeline_magic_model.py)
- [MinerU Pipeline Backend - DeepWiki](https://deepwiki.com/OpenDataLab/MinerU/2.1-pipeline-backend)
- [MinerU: An Open-Source Solution for Precise Document Content Extraction (arXiv:2409.18839)](https://arxiv.org/pdf/2409.18839v1)
- [gfm_adapter.rs 现状（主动绕过版面）](file:///workspace/src/gfm_adapter.rs#L1-L9)
- [reading_order.rs 现状（最大间隙列检测，保留作降级）](file:///workspace/src/reading_order.rs#L110-L139)
- [ocr_engine.rs 版面模型配置](file:///workspace/src/ocr_engine.rs#L189-L214)
