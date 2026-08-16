//! ADR-0009 块驱动阅读序：三级降级链 + 块内排序 + 坐标归一化。
//!
//! 三级降级链：
//! 1. `LayoutElement::order_index` 排序（模型阅读序，对齐 MinerU `index`）
//! 2. `RegionBlock::order_index` + `element_indices`（PP-DocBlockLayout 列级分组）
//! 3. [`super::columns::order_text_regions`]（区域驱动兜底，行为等价现状）

use super::columns::{detect_column_split, order_text_regions, split_columns};
use super::lines::merge_into_paragraphs;
use crate::region::Region;
use oar_ocr::domain::structure::{LayoutElement, LayoutElementType, RegionBlock, StructureResult};

/// 噪声块类型集合（ADR-0009 D4：按类型过滤，对齐 MinerU BlockType 丢弃）。
/// 首版不设置信度阈值（对齐 MinerU 源码：按类型全量分派）。
const NOISE_TYPES: &[LayoutElementType] = &[
    LayoutElementType::Header,
    LayoutElementType::HeaderImage,
    LayoutElementType::Footer,
    LayoutElementType::FooterImage,
    LayoutElementType::Number,
    LayoutElementType::Seal,
];

/// 页内尺度归一化：text_regions（原图尺度）与 layout_elements bbox（模型 resize 尺度）
/// 坐标系不同。返回 `(t_max_x, t_max_y, l_max_x, l_max_y)`，供归一化比较使用。
pub(crate) fn page_scale(page: &StructureResult) -> (f32, f32, f32, f32) {
    let mut tw = 0.0_f32;
    let mut th = 0.0_f32;
    if let Some(regs) = &page.text_regions {
        for r in regs {
            tw = tw.max(r.bounding_box.x_max());
            th = th.max(r.bounding_box.y_max());
        }
    }
    let mut lw = 0.0_f32;
    let mut lh = 0.0_f32;
    for el in &page.layout_elements {
        lw = lw.max(el.bbox.x_max());
        lh = lh.max(el.bbox.y_max());
    }
    (tw, th, lw, lh)
}

/// 归一化判定：text 尺度点 `(cx, cy)` 是否落在 layout 尺度 bbox `lb` 内。
/// 各自除以页内最大值转 [0,1]，消除 text/layout 两套坐标尺度差。
pub(crate) fn norm_membership(
    cx: f32,
    cy: f32,
    (tw, th, lw, lh): (f32, f32, f32, f32),
    lb: &oar_ocr::processors::BoundingBox,
) -> bool {
    if tw <= 0.0 || th <= 0.0 || lw <= 0.0 || lh <= 0.0 {
        return false;
    }
    let tx = cx / tw;
    let ty = cy / th;
    let ix0 = lb.x_min() / lw;
    let ix1 = lb.x_max() / lw;
    let iy0 = lb.y_min() / lh;
    let iy1 = lb.y_max() / lh;
    tx >= ix0 && tx <= ix1 && ty >= iy0 && ty <= iy1
}

/// ADR-0011：块驱动阅读序。三级降级链：
/// 1. `LayoutElement::order_index` 排序（模型阅读序，对齐 MinerU `index`）
/// 2. `RegionBlock::order_index` + `element_indices`（PP-DocBlockLayout 列级分组）
/// 3. 现有 `order_text_regions`（区域驱动兜底，行为等价现状）
///
/// 噪声类型块（Header/Footer/Number/Seal）直接跳过（D4）。
/// 块内：收集中心点落在 bbox 内的 regions → 块内列检测（Q6）→ 段落合并（D3）。
/// 返回段落列表（已是合并后的行）。
///
/// 这是 OCR 通路入口，与 `order_text_regions`（文字层通路入口）同级。
pub fn order_structure(page: &StructureResult, regions: &[Region]) -> Vec<String> {
    if regions.is_empty() {
        return Vec::new();
    }
    // 收集有效块（跳过噪声类型），按 order_index 排序；None 的块排末尾按 bbox.y_min
    let mut blocks: Vec<&LayoutElement> = page
        .layout_elements
        .iter()
        .filter(|el| !NOISE_TYPES.contains(&el.element_type))
        .collect();
    let has_order = blocks.iter().any(|el| el.order_index.is_some());
    if !has_order {
        return fallback_order(page, regions);
    }
    blocks.sort_by(|a, b| match (a.order_index, b.order_index) {
        (Some(i), Some(j)) => i.cmp(&j),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a
            .bbox
            .y_min()
            .partial_cmp(&b.bbox.y_min())
            .unwrap_or(std::cmp::Ordering::Equal),
    });

    let scale = page_scale(page);
    let mut consumed: Vec<bool> = vec![false; regions.len()];
    let mut out = assemble_blocks(&blocks, regions, scale, &mut consumed);
    append_leftover(page, regions, scale, &consumed, &mut out);
    out
}

/// 块级装配（消除 block_driven_order / fallback_order / leftover 的循环重复）：
/// 对每个块收集中心点落在 bbox 内的未消费 regions → 块内列检测 + 段落合并。
fn assemble_blocks<'a>(
    blocks: &[&'a LayoutElement],
    regions: &'a [Region],
    scale: (f32, f32, f32, f32),
    consumed: &mut [bool],
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for blk in blocks {
        let inner_idx: Vec<usize> = regions
            .iter()
            .enumerate()
            .filter(|(i, r)| {
                !consumed[*i] && norm_membership(r.center_x(), r.y_min, scale, &blk.bbox)
            })
            .map(|(i, _)| i)
            .collect();
        if inner_idx.is_empty() {
            continue;
        }
        for &i in &inner_idx {
            consumed[i] = true;
        }
        let inner: Vec<Region> = inner_idx.iter().map(|&i| regions[i].clone()).collect();
        out.extend(merge_into_paragraphs(&order_within_block(&inner)));
    }
    out
}

/// 未被任何块消费的 regions（bbox 不匹配，模型漏检）：追加到末尾，按 y 排序。
/// 排除落在噪声块内的 region（避免页眉/页码混入 leftover）。
fn append_leftover(
    page: &StructureResult,
    regions: &[Region],
    scale: (f32, f32, f32, f32),
    consumed: &[bool],
    out: &mut Vec<String>,
) {
    let noise_bboxes: Vec<&oar_ocr::processors::BoundingBox> = page
        .layout_elements
        .iter()
        .filter(|el| NOISE_TYPES.contains(&el.element_type))
        .map(|el| &el.bbox)
        .collect();
    let mut leftover: Vec<&Region> = regions
        .iter()
        .enumerate()
        .filter(|(i, r)| {
            !consumed[*i]
                && !noise_bboxes
                    .iter()
                    .any(|nb| norm_membership(r.center_x(), r.y_min, scale, nb))
        })
        .map(|(_, r)| r)
        .collect();
    if !leftover.is_empty() {
        leftover.sort_by(|a, b| {
            a.y_min
                .partial_cmp(&b.y_min)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let lines: Vec<(f32, String)> =
            leftover.iter().map(|r| (r.y_min, r.text.clone())).collect();
        out.extend(merge_into_paragraphs(&lines));
    }
}

/// ADR-0009 D2：三级降级链——order_index 全 None 时调用。
fn fallback_order(page: &StructureResult, regions: &[Region]) -> Vec<String> {
    if let Some(rbs) = &page.region_blocks {
        // P0-2：filter_map 携带 order_index 值，排序不再 unwrap
        let mut sorted_rbs: Vec<(&RegionBlock, _)> =
            rbs.iter().filter_map(|rb| rb.order_index.map(|oi| (rb, oi))).collect();
        sorted_rbs.sort_by_key(|&(_, oi)| oi);
        if !sorted_rbs.is_empty() {
            let scale = page_scale(page);
            let mut out: Vec<String> = Vec::new();
            let mut consumed: Vec<bool> = vec![false; regions.len()];
            for &(rb, _) in &sorted_rbs {
                let mut els: Vec<&LayoutElement> = rb
                    .element_indices
                    .iter()
                    .filter_map(|&i| page.layout_elements.get(i))
                    .filter(|el| !NOISE_TYPES.contains(&el.element_type))
                    .collect();
                els.sort_by(|a, b| match (a.order_index, b.order_index) {
                    (Some(i), Some(j)) => i.cmp(&j),
                    _ => a
                        .bbox
                        .y_min()
                        .partial_cmp(&b.bbox.y_min())
                        .unwrap_or(std::cmp::Ordering::Equal),
                });
                out.extend(assemble_blocks(&els, regions, scale, &mut consumed));
            }
            if !out.is_empty() {
                return out;
            }
        }
    }
    // 末选：现有区域驱动
    order_text_regions(regions)
}

/// ADR-0009 D3+Q6：块内排序——列检测收窄到块内 + y 排序。
///
/// 复用 `detect_column_split` 的最大间隙逻辑，但作用域从全页收窄到单块。
/// 单块裹双列（模型把双列正文判成 1 个 Text 块）时分离为左列全→右列全；否则 y 排序。
/// 返回 `(y, text)` 元组，供 `merge_into_paragraphs` 按行距合并。
fn order_within_block(regions: &[Region]) -> Vec<(f32, String)> {
    if regions.is_empty() {
        return Vec::new();
    }
    if let Some(split) = detect_column_split(regions) {
        let (left_refs, right_refs, full_refs) = split_columns(regions, split);
        let mut left: Vec<(f32, String)> = left_refs
            .iter()
            .map(|r| (r.y_min, r.text.clone()))
            .collect();
        let mut right: Vec<(f32, String)> = right_refs
            .iter()
            .map(|r| (r.y_min, r.text.clone()))
            .collect();
        let mut mid: Vec<(f32, String)> = full_refs
            .iter()
            .map(|r| (r.y_min, r.text.clone()))
            .collect();
        left.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        right.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        mid.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        return mid.into_iter().chain(left).chain(right).collect();
    }
    // 单列：纯 y 排序
    let mut v: Vec<(f32, String)> = regions.iter().map(|r| (r.y_min, r.text.clone())).collect();
    v.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    v
}

#[cfg(test)]
mod tests {
    use super::{order_structure, order_within_block};
    use crate::region::Region;
    use oar_ocr::domain::TextRegion;
    use oar_ocr::domain::structure::{LayoutElement, LayoutElementType, StructureResult};
    use oar_ocr::processors::BoundingBox;

    /// 构造 TextRegion：(x_min, y_min, x_max, y_max, 文本)
    fn tr(x0: f32, y0: f32, x1: f32, y1: f32, text: &str) -> TextRegion {
        TextRegion {
            bounding_box: BoundingBox::from_coords(x0, y0, x1, y1),
            text: Some(text.into()),
            ..TextRegion::new(BoundingBox::from_coords(x0, y0, x1, y1))
        }
    }

    /// 构造 layout 块元素：(x_min, y_min, x_max, y_max, 类型, order_index)
    fn block_el(
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        ty: LayoutElementType,
        order: Option<u32>,
    ) -> LayoutElement {
        let mut el = LayoutElement::new(BoundingBox::from_coords(x0, y0, x1, y1), ty, 0.9);
        el.order_index = order;
        el
    }

    /// Region 从 TextRegion 转换（测试辅助）。
    fn regions_of(trs: &[TextRegion]) -> Vec<Region> {
        trs.iter()
            .filter_map(|r| {
                r.text.as_ref().filter(|t| !t.trim().is_empty()).map(|t| {
                    let b = &r.bounding_box;
                    Region::new(
                        b.x_min(),
                        b.x_max(),
                        b.y_min(),
                        b.y_max(),
                        t.as_ref().to_string(),
                    )
                })
            })
            .collect()
    }

    /// ADR-0009 D1：有 order_index 时块驱动排序，噪声块被跳过。
    #[test]
    fn block_driven_orders_by_order_index_skips_noise() {
        // 页面：Header 块（噪声）+ 两个 Text 块（order_index 1 在下、0 在上）
        let page = StructureResult {
            layout_elements: vec![
                block_el(0.0, 0.0, 1000.0, 50.0, LayoutElementType::Header, Some(0)),
                block_el(50.0, 100.0, 950.0, 150.0, LayoutElementType::Text, Some(2)),
                block_el(50.0, 200.0, 950.0, 250.0, LayoutElementType::Text, Some(1)),
            ],
            text_regions: Some(vec![
                tr(0.0, 10.0, 1000.0, 40.0, "页眉噪声"),
                tr(50.0, 110.0, 950.0, 140.0, "正文A"),
                tr(50.0, 210.0, 950.0, 240.0, "正文B"),
            ]),
            ..StructureResult::new("t", 0)
        };
        let regions = regions_of(page.text_regions.as_ref().unwrap());
        let out = order_structure(&page, &regions);
        // 正文B（order=1）先于 正文A（order=2）；页眉噪声被过滤
        assert_eq!(out, vec!["正文B", "正文A"]);
    }

    /// ADR-0009 D2：order_index 全 None → 降级到 RegionBlock；都无 → order_text_regions。
    #[test]
    fn block_driven_fallback_when_no_order_index() {
        // 无 order_index、无 region_blocks → 降级到 order_text_regions（单列 y 排序）
        let page = StructureResult {
            layout_elements: vec![
                block_el(50.0, 200.0, 950.0, 250.0, LayoutElementType::Text, None),
                block_el(50.0, 100.0, 950.0, 150.0, LayoutElementType::Text, None),
            ],
            text_regions: Some(vec![
                tr(50.0, 210.0, 950.0, 240.0, "下"),
                tr(50.0, 110.0, 950.0, 140.0, "上"),
            ]),
            region_blocks: None,
            ..StructureResult::new("t", 0)
        };
        let regions = regions_of(page.text_regions.as_ref().unwrap());
        let out = order_structure(&page, &regions);
        // y 排序：上 先于 下
        assert!(out[0].contains("上"));
        assert!(out[1].contains("下"));
    }

    /// ADR-0009 Q6：单 Text 块裹双列 → 块内列检测分离左右列。
    #[test]
    fn order_within_block_splits_two_columns() {
        // 单块内 6 regions：左列 3 行 + 右列 3 行，x 中心分两簇
        let regions = vec![
            Region::new(50.0, 450.0, 100.0, 110.0, "L1"),
            Region::new(50.0, 450.0, 200.0, 210.0, "L2"),
            Region::new(50.0, 450.0, 300.0, 310.0, "L3"),
            Region::new(550.0, 950.0, 100.0, 110.0, "R1"),
            Region::new(550.0, 950.0, 200.0, 210.0, "R2"),
            Region::new(550.0, 950.0, 300.0, 310.0, "R3"),
        ];
        let out = order_within_block(&regions);
        let texts: Vec<&str> = out.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(texts, vec!["L1", "L2", "L3", "R1", "R2", "R3"]);
    }
}
