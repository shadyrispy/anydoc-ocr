//! ADR-0009 块驱动阅读序：三级降级链 + 块内排序 + 坐标归一化。
//!
//! 三级降级链：
//! 1. `LayoutElement::order_index` 排序（模型阅读序，对齐 MinerU `index`）
//! 2. `RegionBlock::order_index` + `element_indices`（PP-DocBlockLayout 列级分组）
//! 3. [`super::columns::order_text_regions`]（区域驱动兜底，行为等价现状）

use super::columns::{
    detect_column_split, order_text_regions_boxed, sort_by_row_boxed, split_columns,
};
use super::lines::{Line, merge_into_paragraphs};
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

/// #10 切片 5：目录块（Content）专用的**统一基准**归一化判定。
///
/// 与 [`norm_membership`] 的差别只在分母：后者用**各自页内最大值**
/// （text 侧 `tw,th` / layout 侧 `lw,lh`），而这两个 max 常被不同元素撑到
/// 不同大小——实测 nuaa_tupian.pdf 第 3 页：text 侧 `th=1118`，layout 侧被
/// 一个竖排 `AsideText`（y 到 1193）撑成 `lh=1193` → 目录块下缘归一化后被
/// 压小（`1092/1193 = 0.915`），而块内最后三行 `10 改进18` / `10.1 总则…18`
/// / `10.2 不合格和纠正措施…18` 的归一化 y 是 `0.926 / 0.943 / 0.965` →
/// **整段落在块外**，目录尾部漏 3 条（本仓 81 vs MinerU 85）。取两侧 max 作
/// 统一分母即消除失真（该页 `1092/1193` 对 `1078/1193` → 判定回到块内）。
///
/// 只用于 Content：页眉/页脚/表格的判定块都很小，宽松化会把正文误判成家具
/// 吃掉（误判比漏判危险得多），故那些仍走严格的 [`norm_membership`]。
pub(crate) fn norm_membership_union(
    cx: f32,
    cy: f32,
    (tw, th, lw, lh): (f32, f32, f32, f32),
    lb: &oar_ocr::processors::BoundingBox,
) -> bool {
    let w = tw.max(lw);
    let h = th.max(lh);
    if w <= 0.0 || h <= 0.0 {
        return false;
    }
    let tx = cx / w;
    let ty = cy / h;
    let ix0 = lb.x_min() / w;
    let ix1 = lb.x_max() / w;
    let iy0 = lb.y_min() / h;
    let iy1 = lb.y_max() / h;
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
///
/// T3：入口先抽竖排正文（`vertical::order_vertical` 检测窄高条簇，按右→左/自上而下
/// 排序），竖排段落优先输出；其余 regions 走原块驱动排序（`order_structure_block_driven`）。
/// 无竖排时 mask 全 false，行为与原实现等价。
/// 块驱动阅读序（ADR-0011 三级降级链 + T3 竖排前置），**带几何**。
///
/// #11b：此前叫 `order_structure` 且返回 `Vec<String>`——几何在这一步就被拍平
/// 丢了，导致 #11 的 content_list v2 拿不到 bbox（实测 18 个样本覆盖率 1/86）。
/// 现在载体换成 [`Line`]（文本 + 几何），并**不再保留 String 版**：唯一的生产
/// 调用方是 OCR 通路（`gfm_adapter::to_docir`），文字层通路走的是同级的
/// `order_text_regions`，留两版只会让"哪份是真相"产生分叉。
pub(crate) fn order_structure_boxed(page: &StructureResult, regions: &[Region]) -> Vec<Line> {
    if regions.is_empty() {
        return Vec::new();
    }
    let (mut out, vertical_mask) = super::vertical::order_vertical(regions);
    let horiz: Vec<Region> = regions
        .iter()
        .enumerate()
        .filter(|(i, _)| !vertical_mask[*i])
        .map(|(_, r)| r.clone())
        .collect();
    out.extend(order_structure_block_driven(page, &horiz));
    out
}

/// 原块驱动排序主体（T3 重构：被 [`order_structure_boxed`] 包装竖排处理）。
fn order_structure_block_driven(page: &StructureResult, regions: &[Region]) -> Vec<Line> {
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
    let mut out = assemble_blocks(page, &blocks, regions, scale, &mut consumed);
    append_leftover(page, regions, scale, &consumed, &mut out);
    out
}

/// 块级装配（消除 block_driven_order / fallback_order / leftover 的循环重复）：
/// 对每个块收集中心点落在 bbox 内的未消费 regions → 块内列检测 + 段落合并。
///
/// #9 修法 1：块若已带 upstream stitch 好的文本（`LayoutElement.text`），
/// **优先用它**，det/rec 行只在无文本时兜底。原因见 [`stitched_block_text`]。
fn assemble_blocks<'a>(
    page: &StructureResult,
    blocks: &[&'a LayoutElement],
    regions: &'a [Region],
    scale: (f32, f32, f32, f32),
    consumed: &mut [bool],
) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();
    for blk in blocks {
        let inner_idx: Vec<usize> = regions
            .iter()
            .enumerate()
            .filter(|(i, r)| {
                !consumed[*i] && norm_membership(r.center_x(), r.y_min, scale, &blk.bbox)
            })
            .map(|(i, _)| i)
            .collect();
        // #10 切片 5：**目录块跳过 stitch 快路**。
        // `stitched_block_text` 是块级拼接——把块内 det/rec 行拼成少数几坨文本
        // （实测 nuaa_tupian.pdf 目录页：版面 Content 块内 40 个 OCR 行 → stitch
        // 拼成 **1 行**）。目录条目的真值粒度是**行级**（MinerU 逐条输出，实测
        // 85 条），走 stitch 就必然整页一坨，且行级 bbox 与 `no_merge` 围栏都
        // 无从生效。故确证为目录块时改走下面的 det/rec 行兜底：行级文本 +
        // 行级 bbox + `merge_into_paragraphs` 的逐条围栏。
        //
        // 确证信号复用 producer 给的 [`Region::index_member`]（gfm_adapter 按
        // "Content 块内存在点线引导行"打的标），**单一真相源**——此处不再重判
        // 几何/形态，免得两处判据漂移。`inner_idx` 非空为前提：空则跳过后块
        // 无输出（文本丢失），宁可退回 stitch。
        let index_block =
            !inner_idx.is_empty() && inner_idx.iter().any(|&i| regions[i].index_member);
        // 修法 1/2/3：stitch 文本优先。命中时块内 regions **一律标记已消费**
        // ——它们的文本已由 stitch 拼进元素，再留给 leftover 就是重复输出。
        if let Some(sb) = stitched_block_text(page, blk) {
            if index_block {
                // 目录块：只消费 `inner_idx`，**不做** stitch 的"子串宽松消费"
                // ——那会把块外（未被 `inner_idx` 命中）的行也吃掉，而它们既不
                // 进 stitch 输出（本次跳过）也不进 leftover → 静默丢字。留给
                // leftover 兜底才是零丢失。
                for &i in &inner_idx {
                    consumed[i] = true;
                }
            } else {
            let before = consumed.iter().filter(|&&c| c).count();
            for &i in &inner_idx {
                consumed[i] = true;
            }
            // stitch 用 `is_overlapping`（IoA，宽松）匹配 regions，块内收集用
            // `norm_membership`（中心点归一化，严格）——两个口径不一致，实测
            // 整句/公式行常因此**没被 inner_idx 命中**却已被 stitch 拼走。
            // 此时若只消费 inner_idx，leftover 会把同一批文本再输出一遍
            // （formula_mixed 实测：整句与 `V=IR(1)` 各出两次）。故再按
            // "文本已被拼走"消费一次：region 文本是 stitch 文本或其吸收原文
            // 的子串即视为已消费（与 stitch 的宽松匹配同方向）。
            let haystack: Vec<&str> = sb
                .lines
                .iter()
                .map(|s| s.as_str())
                .chain(sb.absorbed.iter().map(|s| s.as_str()))
                .collect();
            for (i, r) in regions.iter().enumerate() {
                if consumed[i] {
                    continue;
                }
                let t = r.text.trim();
                if t.is_empty() {
                    continue;
                }
                if haystack.iter().any(|h| h.contains(t)) {
                    consumed[i] = true;
                }
            }
            // #15 尾巴（重叠块去重）：PP-DocLayout-S 会输出 bbox 交错、内容互
            // 相包含的 Text 块（GJB 9001C 页 24 实测：Text 块 y[197,219] 与
            // y[215,265] 交错，stitch text 都含同一批行——stitching 对每个
            // 元素独立吸收行，重叠块各得一份重复 text）。前面的块已把这些行
            // 消费并输出；本块 inner_idx 为空且宽松消费也没吃到新行 → 本块的
            // stitch 文本必然是已输出内容的重复拼贴，再输出就是整段重行
            // （实测页 24 8.5.3 整段两份）。跳过**输出**、保留消费标记——
            // 被宽松消费吃掉的行确已由前块 stitch 拼走，不消费会被 leftover
            // 重复输出；而真正的块外新行不受影响（inner_idx 非空或宽松消费
            // 有进账时照常输出）。
            if inner_idx.is_empty() && consumed.iter().filter(|&&c| c).count() == before {
                continue;
            }
            // #11b：stitch 文本由**整个块**拼出 → 几何取块 bbox（不是行级框）。
            // 段落在块内按 y 均分不可靠（stitch 给的换行边界没有 y 信息），故整块
            // 一个框；块若给了退化框（0 面积）就记 None，不伪造。
            let blk_box = {
                let b = &blk.bbox;
                let bx = (b.x_min(), b.x_max(), b.y_min(), b.y_max());
                (bx.1 > bx.0 && bx.3 > bx.2).then_some(bx)
            };
            out.extend(
                sb.lines
                    .into_iter()
                    .map(|l| Line { y: 0.0, text: l, bbox: blk_box, font_size: None, no_merge: false }),
            );
            continue;
            }
        }
        if inner_idx.is_empty() {
            continue;
        }
        for &i in &inner_idx {
            consumed[i] = true;
        }
        let inner: Vec<Region> = inner_idx.iter().map(|&i| regions[i].clone()).collect();
        // 目录块：禁列切分（同 [`super::columns::order_text_regions_boxed`] 的
        // 短路，理由见那里）——`order_within_block` 的列检测会把目录页误判双列。
        let ordered = if index_block {
            sort_by_row_boxed(&inner)
        } else {
            order_within_block(&inner)
        };
        out.extend(merge_into_paragraphs(&ordered));
    }
    out
}

/// #9 修法 1/2/3：上游 stitch 已拼好的块文本 → 该块的正文行。
///
/// `None` = 该块无 stitch 文本（走 det/rec 行兜底）；`Some(lines)` = 已定稿的
/// 行（`Some(vec![])` 是"这块不产出正文"，如行内公式与编号——内容已并进别处）。
///
/// **为什么优先用 stitch 文本**（#9 第 0 步取证，真 CLI vs 真 MinerU basic
/// 同件对拍）：行内公式、公式编号这些"行内/行尾对象"被 upstream 拼回了原句
/// （`stitching.rs` 的 `sort_and_join_texts` + `inject_inline_formulas`），而
/// 我们此前**只从 `text_regions` 重建正文**，把拼好的整句扔了 → 行内公式以
/// 孤立行落进正文、被段落合并焊成 `The relationE=m c^{2}` 并与下一段粘连。
/// 这不是模型差距，是我们少用了一路已算好的信息。
///
/// 形态决策落在本函数而不在渲染层的原因：[`order_structure`] 的输出是**字符串
/// 行**（历史接口），`$$` 定界与编号并入只能在"块 → 行"这一步决定；#6 未来把
/// 阅读序 Region 化之后，这两条可以下移到渲染层按 kind 分流。
fn stitched_block_text(page: &StructureResult, blk: &LayoutElement) -> Option<StitchedBlock> {
    use oar_ocr::domain::structure::LayoutElementType as T;

    let label = blk.label.as_deref().unwrap_or("");
    // 行内公式元素：内容已由上游并进 Text 元素（inject 逻辑），且实测其
    // `text` 会被 OCR 匹配污染成整句 → 不输出，防重复。
    if blk.element_type == T::Formula && label.contains("inline") {
        return Some(StitchedBlock::empty());
    }
    // 公式编号：修法 3——就近并入同行 display 公式，不独立成行。
    if blk.element_type == T::FormulaNumber {
        // 编号元素自己的文本也算"已吸收"（被并进公式块），供调用方消费
        // 对应 region、防 leftover 再输出一遍 `(1)`。
        let raw = blk.text.as_deref().unwrap_or("").trim().to_string();
        return Some(StitchedBlock {
            lines: Vec::new(),
            absorbed: if raw.is_empty() { Vec::new() } else { vec![raw] },
        });
    }
    let text = blk.text.as_ref()?.trim();
    if text.is_empty() {
        return None;
    }
    if blk.element_type == T::Formula {
        // 修法 2：行间公式带 `$$ … $$` 定界符（此前裸 LaTeX/rec 文本，无定界）。
        // 修法 3：同行右侧的公式编号并进公式块（`\tag{…}`，对齐 MinerU
        // `optimize_hybrid_formula_number_blocks` 的形态），不再独立成行。
        let num = nearest_formula_number(page, blk);
        let tag = num
            .as_ref()
            .map(|(n, _)| format!(" \\tag{{{n}}}"))
            .unwrap_or_default();
        let absorbed = num.map(|(_, raw)| raw).into_iter().collect();
        return Some(StitchedBlock {
            lines: vec![format!("$$ {text}{tag} $$")],
            absorbed,
        });
    }
    // Text（及其它有 text 的元素）：按 stitch 的换行切行（段落边界由上游
    // 几何判定给出，比层内段落合并更准，且 `order_index` 从此真正生效）。
    let lines: Vec<String> = text
        .split('\n')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    Some(StitchedBlock { lines, absorbed: Vec::new() })
}

/// stitch 产出的块文本：`lines` 是要输出的正文行，`absorbed` 是**已被并进
/// `lines` 的原文**（公式编号等）——不输出，只用于消费对应 region。
struct StitchedBlock {
    lines: Vec<String>,
    absorbed: Vec<String>,
}

impl StitchedBlock {
    fn empty() -> Self {
        Self { lines: Vec::new(), absorbed: Vec::new() }
    }
}

/// 修法 3：取与公式块**同一 y 带、且在其右侧**的公式编号文本（`(1)` → `1`）。
///
/// 判据对齐 MinerU 的几何口径：中心 y 落在公式块垂直范围内（带 25% 容差），
/// 且编号左缘在公式块水平中线右侧。多个候选取最近的。
///
/// 返回 `(编号正文, 原文)`：正文用于 `\tag{…}`，原文用于消费对应 region。
fn nearest_formula_number(
    page: &StructureResult,
    formula: &LayoutElement,
) -> Option<(String, String)> {
    let fy0 = formula.bbox.y_min();
    let fy1 = formula.bbox.y_max();
    let tol = (fy1 - fy0) * 0.25;
    let mid_x = (formula.bbox.x_min() + formula.bbox.x_max()) / 2.0;
    let mut best: Option<(f32, String, String)> = None;
    for el in &page.layout_elements {
        if el.element_type != oar_ocr::domain::structure::LayoutElementType::FormulaNumber {
            continue;
        }
        let Some(raw) = el.text.as_ref() else { continue };
        let cy = (el.bbox.y_min() + el.bbox.y_max()) / 2.0;
        if cy < fy0 - tol || cy > fy1 + tol || el.bbox.x_min() < mid_x {
            continue;
        }
        let n = raw.trim().trim_matches('$').trim().trim_matches('(').trim_end_matches(')');
        let n = n.trim();
        if n.is_empty() {
            continue;
        }
        let d = (cy - (fy0 + fy1) / 2.0).abs();
        if best.as_ref().map_or(true, |(bd, _, _)| d < *bd) {
            best = Some((d, n.to_string(), raw.trim().to_string()));
        }
    }
    best.map(|(_, n, raw)| (n, raw))
}

/// 未被任何块消费的 regions（bbox 不匹配，模型漏检）：追加到末尾，按 y 排序。
/// 排除落在噪声块内的 region（避免页眉/页码混入 leftover）。
fn append_leftover(
    page: &StructureResult,
    regions: &[Region],
    scale: (f32, f32, f32, f32),
    consumed: &[bool],
    out: &mut Vec<Line>,
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
        let lines: Vec<Line> = leftover.iter().map(|r| Line::from_region(r)).collect();
        out.extend(merge_into_paragraphs(&lines));
    }
}

/// ADR-0009 D2：三级降级链——order_index 全 None 时调用。
fn fallback_order(page: &StructureResult, regions: &[Region]) -> Vec<Line> {
    if let Some(rbs) = &page.region_blocks {
        // P0-2：filter_map 携带 order_index 值，排序不再 unwrap
        let mut sorted_rbs: Vec<(&RegionBlock, _)> =
            rbs.iter().filter_map(|rb| rb.order_index.map(|oi| (rb, oi))).collect();
        sorted_rbs.sort_by_key(|&(_, oi)| oi);
        if !sorted_rbs.is_empty() {
            let scale = page_scale(page);
            let mut out: Vec<Line> = Vec::new();
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
                out.extend(assemble_blocks(page, &els, regions, scale, &mut consumed));
            }
            if !out.is_empty() {
                return out;
            }
        }
    }
    // 末选：现有区域驱动。#11b 曾封成无几何行（`Line::from_texts`，行为不变
    // 的保守取态）；#10 INDEX 票实测发现 OCR 真实链路 `order_index`/`region_blocks`
    // 均无人填充（oar-core 不产、本仓不补）→ **末选就是常态路径**，无几何让
    // content_list v2 的 bbox 投影恒退化 0 框、版面 Content 回贴无从判定。
    // 换带几何的真相函数（`order_text_regions` 本就是它的 text 投影，行序
    // 逐行一致）→ 文本输出不变，几何从 0 框变真实框。
    order_text_regions_boxed(regions)
}

/// ADR-0009 D3+Q6：块内排序——列检测收窄到块内 + y 排序。
///
/// 复用 `detect_column_split` 的最大间隙逻辑，但作用域从全页收窄到单块。
/// 单块裹双列（模型把双列正文判成 1 个 Text 块）时分离为左列全→右列全；否则 y 排序。
/// 返回 [`Line`]（`y` + 文本 + **几何**），供 `merge_into_paragraphs` 按行距合并。
fn order_within_block(regions: &[Region]) -> Vec<Line> {
    if regions.is_empty() {
        return Vec::new();
    }
    let sort_by_y = |v: &mut [Line]| {
        v.sort_by(|a, b| a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal));
    };
    if let Some(split) = detect_column_split(regions) {
        let (left_refs, right_refs, full_refs) = split_columns(regions, split);
        let mut left: Vec<Line> = left_refs.iter().map(|r| Line::from_region(r)).collect();
        let mut right: Vec<Line> = right_refs.iter().map(|r| Line::from_region(r)).collect();
        let mut mid: Vec<Line> = full_refs.iter().map(|r| Line::from_region(r)).collect();
        sort_by_y(&mut left);
        sort_by_y(&mut right);
        sort_by_y(&mut mid);
        return mid.into_iter().chain(left).chain(right).collect();
    }
    // 单列：纯 y 排序
    let mut v: Vec<Line> = regions.iter().map(Line::from_region).collect();
    sort_by_y(&mut v);
    v
}

#[cfg(test)]
mod tests {
    use super::{Line, order_structure_boxed, order_within_block};
    use crate::region::Region;
    use oar_ocr::domain::TextRegion;
    use oar_ocr::domain::structure::{LayoutElement, LayoutElementType, StructureResult};
    use oar_ocr::processors::BoundingBox;

    /// #11b：断言仍按**文本**写（排序语义没变），几何另有用例单独钉。
    fn texts(lines: Vec<Line>) -> Vec<String> {
        lines.into_iter().map(|l| l.text).collect()
    }

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

    /// 带 stitch 文本与 label 的块元素（#9 修法 1/2/3 用）。
    fn el_text(
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        ty: LayoutElementType,
        order: Option<u32>,
        label: &str,
        text: &str,
    ) -> LayoutElement {
        let mut el = block_el(x0, y0, x1, y1, ty, order);
        el.label = Some(label.into());
        el.text = Some(text.into());
        el
    }

    /// #9 修法 1：Text 元素带 stitch 文本 → 直接用它（段落边界由上游几何判定），
    /// 不再从 regions 重建（此前会把行内公式句与邻居焊在一起）。
    #[test]
    fn stitched_text_preferred_over_region_merge() {
        let page = StructureResult {
            layout_elements: vec![el_text(
                0.0,
                0.0,
                1000.0,
                100.0,
                LayoutElementType::Text,
                Some(0),
                "text",
                "整句带行内公式\n第二段",
            )],
            text_regions: Some(vec![
                tr(10.0, 10.0, 900.0, 30.0, "整句带行内公式"),
                tr(10.0, 50.0, 900.0, 70.0, "第二段"),
            ]),
            region_blocks: None,
            ..StructureResult::new("t", 0)
        };
        let regions = regions_of(page.text_regions.as_ref().unwrap());
        let out = texts(order_structure_boxed(&page, &regions));
        assert_eq!(out, vec!["整句带行内公式", "第二段"]);
    }

    /// #15 尾巴（重叠块去重）：版面输出 bbox 交错、内容互相包含的 Text 块时
    /// （stitching 对每个元素独立吸收行 → 重叠块各得一份重复 text），后面的
    /// 块 inner_idx 为空且宽松消费无进账 → 其 stitch 文本必是已输出内容的
    /// 重复拼贴，跳过输出。GJB 9001C 页 24 实测：8.5.3 段整段两份。
    #[test]
    fn overlapping_block_with_fully_consumed_lines_is_skipped() {
        // 块1 y[197,219] 行 A/B 的 stitch；块2 y[215,265] 与块1 交错，stitch
        // text 是同一批行的重拼。两行的 y_min 都落在块1 bbox 内（norm 口径），
        // 块2 处理时 inner_idx 为空 → 不再输出。
        let page = StructureResult {
            layout_elements: vec![
                el_text(
                    80.0, 197.0, 509.0, 219.0,
                    LayoutElementType::Text,
                    Some(0),
                    "text",
                    "组织应爱护顾客财产。对构成产品和服务一部分的供方财产，组织应予以识别、保护和防",
                ),
                el_text(
                    80.0, 215.0, 724.0, 265.0,
                    LayoutElementType::Text,
                    Some(1),
                    "text",
                    "组织应爱护顾客财产。\n对构成产品和服务一部分的供方财产，组织应予以识别、保护和防护。",
                ),
            ],
            text_regions: Some(vec![
                tr(111.0, 203.0, 506.0, 213.0, "组织应爱护顾客财产。"),
                tr(111.0, 225.0, 721.0, 231.0, "对构成产品和服务一部分的供方财产，组织应予以识别、保护和防"),
            ]),
            region_blocks: None,
            ..StructureResult::new("t", 0)
        };
        let regions = regions_of(page.text_regions.as_ref().unwrap());
        let out = texts(order_structure_boxed(&page, &regions));
        // 只出块1 一份；块2 的重复拼贴被闸掉
        assert_eq!(
            out,
            vec!["组织应爱护顾客财产。对构成产品和服务一部分的供方财产，组织应予以识别、保护和防"],
            "got: {out:?}"
        );
    }

    /// 对照（防误杀）：bbox 与前块重叠的正常相邻块（stitch 只有新内容，
    /// inner_idx 非空）→ 照常输出，闸不误伤。
    #[test]
    fn overlapping_block_with_new_lines_still_renders() {
        let page = StructureResult {
            layout_elements: vec![
                el_text(
                    80.0, 197.0, 509.0, 219.0,
                    LayoutElementType::Text,
                    Some(0),
                    "text",
                    "第一句在前面的块",
                ),
                // 块2 bbox 与块1 交错（y[215,265] vs y[197,219]）但内容是
                // 新段落——真实场景的相邻段落块微重叠
                el_text(
                    80.0, 215.0, 724.0, 265.0,
                    LayoutElementType::Text,
                    Some(1),
                    "text",
                    "块二自己的新行",
                ),
            ],
            text_regions: Some(vec![
                tr(111.0, 203.0, 506.0, 213.0, "第一句在前面的块"),
                tr(111.0, 230.0, 721.0, 240.0, "块二自己的新行"),
            ]),
            region_blocks: None,
            ..StructureResult::new("t", 0)
        };
        let regions = regions_of(page.text_regions.as_ref().unwrap());
        let out = texts(order_structure_boxed(&page, &regions));
        assert_eq!(
            out,
            vec!["第一句在前面的块", "块二自己的新行"],
            "inner_idx 非空（新行在块内）→ stitch 照常输出",
        );
    }

    /// #9 修法 2/3：display 公式带 `$$ … $$` 且同行右侧编号并入 `\tag{…}`；
    /// inline 公式元素（内容已在 Text 里）不输出，防重复。
    #[test]
    fn display_formula_delimited_and_number_absorbed() {
        let page = StructureResult {
            layout_elements: vec![
                el_text(
                    100.0,
                    100.0,
                    600.0,
                    160.0,
                    LayoutElementType::Formula,
                    Some(0),
                    "display_formula",
                    "V=IR",
                ),
                el_text(
                    640.0,
                    110.0,
                    700.0,
                    150.0,
                    LayoutElementType::FormulaNumber,
                    None,
                    "formula_number",
                    "(1)",
                ),
                el_text(
                    100.0,
                    200.0,
                    600.0,
                    260.0,
                    LayoutElementType::Formula,
                    Some(1),
                    "inline_formula",
                    "E=mc2",
                ),
            ],
            text_regions: Some(vec![
                tr(100.0, 110.0, 600.0, 150.0, "V=IR"),
                tr(640.0, 120.0, 700.0, 140.0, "(1)"),
            ]),
            region_blocks: None,
            ..StructureResult::new("t", 0)
        };
        let regions = regions_of(page.text_regions.as_ref().unwrap());
        let out = texts(order_structure_boxed(&page, &regions));
        // 公式带定界符 + 编号并入；编号不独立成行；inline 公式不出
        assert_eq!(out, vec!["$$ V=IR \\tag{1} $$"], "got: {out:?}");
    }

    /// 元素无 stitch 文本 → 兜底走 region 装配（修法 1 前的行为，不得回归）。
    #[test]
    fn no_stitched_text_falls_back_to_regions() {
        let page = StructureResult {
            layout_elements: vec![block_el(
                0.0,
                0.0,
                1000.0,
                200.0,
                LayoutElementType::Text,
                Some(0),
            )],
            // 前三行紧密、末行远离：段落合并的阈值是**行距中位数 ×1.5**，
            // 故必须给足样本数才能让末行独立成段（两行时中位数即唯一间距，恒合并）。
            text_regions: Some(vec![
                tr(10.0, 10.0, 900.0, 25.0, "上"),
                tr(10.0, 30.0, 900.0, 45.0, "中"),
                tr(10.0, 50.0, 900.0, 65.0, "下"),
                tr(10.0, 400.0, 900.0, 415.0, "末"),
            ]),
            region_blocks: None,
            ..StructureResult::new("t", 0)
        };
        let regions = regions_of(page.text_regions.as_ref().unwrap());
        assert_eq!(texts(order_structure_boxed(&page, &regions)), vec!["上中下", "末"]);
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
        let out = texts(order_structure_boxed(&page, &regions));
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
        let out = texts(order_structure_boxed(&page, &regions));
        // y 排序：上 先于 下
        assert!(out[0].contains("上"));
        assert!(out[1].contains("下"));
    }

    /// #10 切片 5：目录块 **跳过 stitch 快路**——`stitched_block_text` 是块级
    /// 拼接（实测把目录页 40 个 OCR 行拼成 1 坨），目录条目必须行级粒度。
    #[test]
    fn index_block_skips_stitched_block_text() {
        fn page_with_content_block(text: &str) -> StructureResult {
            StructureResult {
                layout_elements: vec![el_text(
                    0.0,
                    100.0,
                    1000.0,
                    400.0,
                    LayoutElementType::Content,
                    Some(0),
                    "content",
                    text,
                )],
                text_regions: Some(vec![
                    tr(10.0, 110.0, 900.0, 130.0, "前言……IV"),
                    tr(10.0, 160.0, 900.0, 180.0, "引言……V"),
                    tr(10.0, 210.0, 900.0, 230.0, "1 范围……1"),
                ]),
                region_blocks: None,
                ..StructureResult::new("t", 0)
            }
        }
        let stitched = "前言……IV 引言……V 1 范围……1"; // 上游拼好的整坨
        let page = page_with_content_block(stitched);
        let mut regions = regions_of(page.text_regions.as_ref().unwrap());
        for r in regions.iter_mut() {
            r.index_member = true; // producer（gfm_adapter）按"块内有点线行"打的标
        }
        let out = texts(order_structure_boxed(&page, &regions));
        assert_eq!(out.len(), 3, "目录块必须逐条，实得 {out:?}");
        assert!(
            !out.iter().any(|t| t.contains("前言……IV 引言")),
            "不该出现 stitch 整坨：{out:?}"
        );

        // 对照组：不是目录块（无 `index_member`）→ 照旧走 stitch 快路（1 条整坨）
        let page2 = page_with_content_block(stitched);
        let regions2 = regions_of(page2.text_regions.as_ref().unwrap());
        let out2 = texts(order_structure_boxed(&page2, &regions2));
        assert_eq!(out2, vec![stitched], "非目录块仍应优先 stitch 文本");
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
        let texts: Vec<&str> = out.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, vec!["L1", "L2", "L3", "R1", "R2", "R3"]);
        // #11b：几何随行带出（列序分离后每行仍是自己那一行的框）
        assert_eq!(out[0].bbox, Some((50.0, 450.0, 100.0, 110.0)));
        assert_eq!(out[3].bbox, Some((550.0, 950.0, 100.0, 110.0)));
    }
}
