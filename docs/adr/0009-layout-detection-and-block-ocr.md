# ADR-0009：充分利用 oar_ocr 版面模型（Header/Footer 过滤 + 分块输出）

- 状态：拟议
- 日期：2026-08-15
- 决策者：anydoc-ocr 维护者
- 上下文：ADR-0008（图像直提）落地后，GJB 9001C 对比测试暴露准确率差距——anydoc-ocr 77%，MinerU 99.6%。初版 ADR-0009 误判"我们无版面检测"，核查后发现：**oar_ocr 已提供完整版面能力（PP-DocLayout + 28 类 LayoutElementType），但我们主动绕过了 Header/Footer 过滤**。

## 背景

### 现状核查（关键修正）

[ocr_engine.rs](file:///workspace/src/ocr_engine.rs#L189-L214) 的 `build_analyzer` 已配置版面模型：

```rust
OARStructureBuilder::new(model_path(layout_model))  // PicoDet-Layout
    .layout_model_name(layout_name)
    .with_ocr(...)
    .with_table_classification(...)
    .with_table_structure_recognition(...)
    .with_document_orientation(...)
    .build()
```

[StructureResult](https://docs.rs/oar-ocr/0.9.1/oar_ocr/domain/structure/struct.StructureResult.html) 已返回完整版面数据：
- `layout_elements: Vec<LayoutElement>` —— 28 类语义块（含 `Header`/`Footer`/`HeaderImage`/`FooterImage`/`Number`/`Seal`）
- `region_blocks: Option<Vec<RegionBlock>>` —— 层级分组（PP-DocBlockLayout，列/节）
- `tables: Vec<TableResult>` / `formulas: Vec<FormulaResult>`
- `text_regions: Option<Vec<TextRegion>>` —— OCR 文本区域

[gfm_adapter.rs](file:///workspace/src/gfm_adapter.rs#L1-L9) **主动绕过版面语义分类**：

> "版面模型（PP-DocLayout）常把整页图片型文档误判为 Header/Footer，而 to_markdown() 会跳过这些类型，导致正文丢失。**主路径直接读取 OCR 的 text_regions（按阅读顺序拼接），而非依赖版面语义分类。**"

### 差距根因（修正后）

| 环节 | MinerU | anydoc-ocr 现状 | 差距影响 |
|------|--------|----------------|---------|
| 版面检测模型 | DocLayout-YOLO | **PicoDet-Layout（已用）** | 模型层无差距 |
| Header/Footer 过滤 | 按 block 类型丢弃 | **完全未用，绕过版面** | 水印/页眉混入正文 → 插入错误 3330 |
| OCR 粒度 | 分块裁剪 OCR | 整页 OCR（text_regions 拼接） | 水印文字当正文识别 |
| 阅读序 | 版面块 order_index | 最大间隙启发式 | 目录点线拆碎 |
| 标题层级 | 版面模型判定 | 仅用 is_title()，编号正则辅助 | 部分使用 |

**根因不是"缺模型"，是"有模型但不敢用"**——因误判 Header/Footer 会丢正文，索性全绕过。

## 决策

### D1：选择性启用版面过滤，而非全量绕过

**不全量信任版面分类**（保留现有 text_regions 主路径），但**选择性过滤高置信度的干扰块**：

```rust
// gfm_adapter.rs 改造：text_regions 拼接前，过滤落在 Header/Footer/FooterImage/Number/Seal 块内的文本
fn filter_noise_regions(page: &StructureResult, regions: &[TextRegion]) -> Vec<TextRegion> {
    let noise_types = [
        LayoutElementType::Header, LayoutElementType::HeaderImage,
        LayoutElementType::Footer, LayoutElementType::FooterImage,
        LayoutElementType::Number, LayoutElementType::Seal,
    ];
    let scale = page_scale(page);
    regions.iter().filter(|r| {
        let cx = (r.bounding_box.x_min() + r.bounding_box.x_max()) / 2.0;
        let cy = (r.bounding_box.y_min() + r.bounding_box.y_max()) / 2.0;
        // 归一化后检查是否落在噪声块内
        !page.layout_elements.iter()
            .filter(|el| noise_types.contains(&el.element_type) && el.confidence > 0.7)
            .any(|el| norm_membership(cx, cy, scale, &el.bbox))
    }).cloned().collect()
}
```

**置信度阈值 0.7**：只过滤高置信度噪声，避免误杀正文（解决"整页被判 Header"的旧问题）。

**降级保护**：若过滤后某页文本量 < 原始 30%，判定版面误判严重，回退不过滤（保正文不丢）。

### D2：阅读序改用 region_blocks（PP-DocBlockLayout）

oar_ocr 的 `RegionBlock` 已提供层级阅读序（`order_index` + `element_indices`）：

```rust
// 替代 reading_order.rs 的最大间隙启发式
fn order_by_regions(page: &StructureResult) -> Vec<String> {
    if let Some(blocks) = &page.region_blocks {
        let mut sorted = blocks.iter()
            .filter(|b| b.order_index.is_some())
            .sorted_by_key(|b| b.order_index.unwrap())
            .collect();
        // 按 region 顺序输出其内部 elements 的文本
        // ...
    } else {
        // 降级：现有 order_text_regions 启发式
        order_text_regions(&regions)
    }
}
```

保留 `reading_order.rs` 作为降级（region_blocks 为 None 或异常时）。

### D3：不引入新模型（否决初版 D1）

初版提议接入 DocLayout-YOLO，**核查后否决**：
- oar_ocr 已绑定 PicoDet-Layout（PP-StructureV3 生态），同源 PaddleOCR
- 换 DocLayout-YOLO 需绕过 oar_ocr 的版面通路，自行预处理/NMS/坐标反映射，重复造轮子
- PicoDet-Layout 的 28 类标签已覆盖 Header/Footer/Number/Seal 等噪声类型，能力足够
- 真正的问题是**配置/使用策略**，非模型能力

### D4：分块 OCR（P2，非首版）

oar_ocr 当前是整页 OCR → text_regions。分块 OCR 需绕过 oar_ocr 的 OCR 通路，自行裁剪 block 后调用底层 det/rec。成本高，首版不做——D1 的噪声过滤已能解决 90% 插入错误。

## 实施计划

### 阶段 1：Header/Footer 噪声过滤（gfm_adapter.rs）

- 实现 `filter_noise_regions`：按版面块类型 + 置信度过滤 text_regions
- 降级保护：过滤后文本量 <30% 则回退
- 验证：nuaa.pdf 插入错误从 3330 降到 <1000；正文不丢失

### 阶段 2：region_blocks 阅读序（reading_order.rs）

- 优先用 `RegionBlock::order_index` 排序
- 降级到现有最大间隙启发式
- 验证：目录段数从 222 降到接近 GT 的 177

### 阶段 3：对比测试

- sdoac_scan.pdf 重跑，对比 CER（预估 23.56% → <10%）
- 耗时无增加（版面模型已在跑，仅利用其输出）
- golden 测试更新

## 风险与缓解

| 风险 | 影响 | 缓解 |
|------|------|------|
| 版面误判 Header 导致正文丢失 | 旧问题复发 | 置信度阈值 0.7 + 过滤后文本量 <30% 回退 |
| region_blocks 为 None（旧模型/简单页） | 阅读序降级 | 保留最大间隙启发式作为降级 |
| 噪声块边界不准，过滤误伤 | 邻近正文被滤 | bbox 收缩 10% 再判包含关系 |
| 不同文档版面模型表现不一 | 部分文档过滤无效 | 置信度阈值可配（`ANYDOC_NOISE_CONF` 环境变量） |

## 非目标

- 不引入 DocLayout-YOLO（oar_ocr 的 PicoDet-Layout 已够用）
- 不做分块 OCR（首版靠过滤解决，分块成本高）
- 不做 middle_json 结构化（P2 再考虑）
- 不做公式识别（MFD/MFR）

## 引用

- [oar_ocr StructureResult](https://docs.rs/oar-ocr/0.9.1/oar_ocr/domain/structure/struct.StructureResult.html)
- [oar_ocr LayoutElement + 28 类标签](https://docs.rs/oar-ocr/0.9.1/oar_ocr/domain/structure/enum.LayoutElementType.html)
- [oar_ocr RegionBlock（PP-DocBlockLayout 层级阅读序）](https://docs.rs/oar-ocr/0.9.1/oar_ocr/domain/structure/struct.RegionBlock.html)
- [MinerU Pipeline Backend - DeepWiki](https://deepwiki.com/OpenDataLab/MinerU/2.1-pipeline-backend)
- [gfm_adapter.rs 现状（主动绕过版面）](file:///workspace/src/gfm_adapter.rs#L1-L9)
- [ocr_engine.rs 版面模型配置](file:///workspace/src/ocr_engine.rs#L189-L214)
