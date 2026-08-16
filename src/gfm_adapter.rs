//! StructureResult → GFM（图片型 PDF/OFD 的 OCR 结果）
//!
//! 主路径直接读取 OCR 的 `text_regions`（按阅读顺序拼接），而非依赖
//! `StructureResult::to_markdown()`。原因：版面模型（PP-DocLayout）常把
//! 整页图片型文档误判为 `Header`/`Footer`，而 `to_markdown()` 会跳过这些
//! 类型，导致正文丢失。文本区域是 OCR 的直接、可靠结果，不受版面语义分类影响。
//! 表格区域单独用 `html_structure` 输出，并剔除落在表格内的文本区域以防重复。
//!
//! 阅读顺序由公共模块 `crate::reading_order` 还原（双列感知），与文字层通路共用。
//!
//! ## Image 块表格补救（A'）
//! 版面模型对"超大表格"（接近整页高、密集多列，如 GJB 标准的附录表）会误判为
//! `Image`（figure）而非 `Table`，导致 `page.tables` 为空、不出 `<table>`。
//! 补救：收集 Image 块内的 text_regions → 网格重建（复用 `crate::table_grid`），
//! 跨页续接合并。防误判见 `reconstruct_image_table`。
use crate::emitter::{DocumentEmitter, FlushFormat};
use crate::reading_order::{order_text_regions, postprocess_lines, title_level};
use crate::region::Region;
use crate::table_grid::{self, TableGrid};
use oar_ocr::domain::structure::{LayoutElement, LayoutElementType, RegionBlock, StructureResult, TableResult};

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

/// 双栏长文本判伪表启发式核心：恰为 2 列、且非空文本中"长文本"占比超过 60%。
///
/// 长文本定义：≥15 字符，或以 。，；： 结尾（真表格单元格通常为短字段/数字）。
/// `texts` 须为 trim 后的非空文本；命中即视为双栏散文而非真表格。
fn is_two_col_prose_like(cols: usize, texts: &[&str]) -> bool {
    if cols != 2 || texts.is_empty() {
        return false;
    }
    let long = texts
        .iter()
        .filter(|t| t.chars().count() >= 15 || t.ends_with(['。', '，', '；', '：']))
        .count();
    long as f32 / texts.len() as f32 > 0.6
}

/// 判断表格是否为版面模型误判的"伪表格"（典型：双栏正文被识别成 2 列表格）。
///
/// 确定性规则，任一命中即拒绝（返回 true）：
/// - 无 cells 且无 html_structure → 无可用内容；
/// - 行数或列数 < 2 → 非真实表格；
/// - 恰好 2 列，且超过 60% 非空单元格是"长文本"（≥15 字符，或以 。，；： 结尾）
///   → 双栏正文特征（真表格的单元格通常为短字段/数字）。
fn is_false_positive_table(table: &TableResult) -> bool {
    if table.cells.is_empty() && table.html_structure.is_none() {
        return true;
    }
    let mut n_rows = 0usize;
    let mut n_cols = 0usize;
    for c in &table.cells {
        n_rows = n_rows.max(c.row.map_or(0, |r| r + 1));
        n_cols = n_cols.max(c.col.map_or(0, |c| c + 1));
    }
    if n_rows < 2 || n_cols < 2 {
        return true;
    }
    let non_empty: Vec<&str> = table
        .cells
        .iter()
        .filter_map(|c| c.text.as_ref().map(|t| t.trim()).filter(|t| !t.is_empty()))
        .collect();
    if is_two_col_prose_like(n_cols, &non_empty) {
        return true;
    }
    false
}

/// 页内尺度：text_regions（原图尺度）与 layout_elements bbox（模型 resize 尺度）
/// 坐标系不同。返回 `(t_max_x, t_max_y, l_max_x, l_max_y)`，供归一化比较使用。
fn page_scale(page: &StructureResult) -> (f32, f32, f32, f32) {
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

/// ADR-0009 D1：块驱动阅读序（对齐 MinerU：iterate blocks in model order, dispatch by type）。
///
/// 三级降级链（Q2）：
/// 1. `LayoutElement::order_index` 排序（模型阅读序，对齐 MinerU `index`）
/// 2. `RegionBlock::order_index` + `element_indices`（PP-DocBlockLayout 列级分组）
/// 3. 现有 `order_text_regions`（区域驱动兜底，行为等价现状）
///
/// 噪声类型块（Header/Footer/Number/Seal）直接跳过（D4，对齐 MinerU 按 BlockType 丢弃）。
/// 块内：收集中心点落在 bbox 内的 regions → 块内列检测（Q6）→ 段落合并（D3）。
/// 返回段落列表（已是合并后的行）。
fn block_driven_order(page: &StructureResult, regions: &[Region]) -> Vec<String> {
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
    blocks.sort_by(|a, b| {
        match (a.order_index, b.order_index) {
            (Some(i), Some(j)) => i.cmp(&j),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a
                .bbox
                .y_min()
                .partial_cmp(&b.bbox.y_min())
                .unwrap_or(std::cmp::Ordering::Equal),
        }
    });

    let scale = page_scale(page);
    let mut out: Vec<String> = Vec::new();
    let mut consumed: Vec<bool> = vec![false; regions.len()];
    for blk in &blocks {
        // 块内收集：中心点落在 bbox 内、且未被前一块消费的 regions
        let inner_idx: Vec<usize> = regions
            .iter()
            .enumerate()
            .filter(|(i, r)| {
                !consumed[*i]
                    && norm_membership(r.center_x(), r.y_min, scale, &blk.bbox)
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
        // 块内列检测（Q6）+ y 排序 + 段落合并（D3）
        let lines = order_within_block(&inner);
        out.extend(merge_into_paragraphs(&lines));
    }
    // 未被任何块消费的 regions（bbox 不匹配，模型漏检）：追加到末尾，按 y 排序。
    // 但排除落在噪声块内的 region（避免页眉/页码混入 leftover）。
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
    out
}

/// ADR-0009 D2：三级降级链——order_index 全 None 时调用。
///
/// 次选 RegionBlock（PP-DocBlockLayout 列级分组）：按 region order_index 排序，
/// 每个 region 内的 elements（按 element_indices 取）块内排序。
/// 末选：现有 `order_text_regions`（区域驱动兜底，行为等价现状）。
fn fallback_order(page: &StructureResult, regions: &[Region]) -> Vec<String> {
    if let Some(rbs) = &page.region_blocks {
        let mut sorted_rbs: Vec<&RegionBlock> = rbs
            .iter()
            .filter(|rb| rb.order_index.is_some())
            .collect();
        sorted_rbs.sort_by_key(|rb| rb.order_index.unwrap());
        if !sorted_rbs.is_empty() {
            let scale = page_scale(page);
            let mut out: Vec<String> = Vec::new();
            let mut consumed: Vec<bool> = vec![false; regions.len()];
            for rb in &sorted_rbs {
                // region 内的 elements 按 element_indices 取出，再按各自 order_index/bbox 排序
                let mut els: Vec<&LayoutElement> = rb
                    .element_indices
                    .iter()
                    .filter_map(|&i| page.layout_elements.get(i))
                    .filter(|el| !NOISE_TYPES.contains(&el.element_type))
                    .collect();
                els.sort_by(|a, b| {
                    match (a.order_index, b.order_index) {
                        (Some(i), Some(j)) => i.cmp(&j),
                        _ => a
                            .bbox
                            .y_min()
                            .partial_cmp(&b.bbox.y_min())
                            .unwrap_or(std::cmp::Ordering::Equal),
                    }
                });
                for el in &els {
                    let inner_idx: Vec<usize> = regions
                        .iter()
                        .enumerate()
                        .filter(|(i, r)| {
                            !consumed[*i]
                                && norm_membership(r.center_x(), r.y_min, scale, &el.bbox)
                        })
                        .map(|(i, _)| i)
                        .collect();
                    if inner_idx.is_empty() {
                        continue;
                    }
                    for &i in &inner_idx {
                        consumed[i] = true;
                    }
                    let inner: Vec<Region> =
                        inner_idx.iter().map(|&i| regions[i].clone()).collect();
                    let lines = order_within_block(&inner);
                    out.extend(merge_into_paragraphs(&lines));
                }
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
/// 复用 `reading_order::detect_column_split` 的最大间隙逻辑，但作用域从全页收窄到单块。
/// 单块裹双列（模型把双列正文判成 1 个 Text 块）时分离为左列全→右列全；否则 y 排序。
/// 返回 `(y, text)` 元组，供 `merge_into_paragraphs` 按行距合并。
fn order_within_block(regions: &[Region]) -> Vec<(f32, String)> {
    if regions.is_empty() {
        return Vec::new();
    }
    // 块内列检测：复用 reading_order 的最大间隙判定
    if let Some(split) = crate::reading_order::detect_column_split(regions) {
        let page_w = Region::page_w(regions);
        let mut left: Vec<(f32, String)> = regions
            .iter()
            .filter(|r| !r.is_full_width(page_w) && r.center_x() < split)
            .map(|r| (r.y_min, r.text.clone()))
            .collect();
        let mut right: Vec<(f32, String)> = regions
            .iter()
            .filter(|r| !r.is_full_width(page_w) && r.center_x() >= split)
            .map(|r| (r.y_min, r.text.clone()))
            .collect();
        let mut mid: Vec<(f32, String)> = regions
            .iter()
            .filter(|r| r.is_full_width(page_w))
            .map(|r| (r.y_min, r.text.clone()))
            .collect();
        left.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        right.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        mid.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        return mid.into_iter().chain(left).chain(right).collect();
    }
    // 单列：纯 y 排序
    let mut v: Vec<(f32, String)> =
        regions.iter().map(|r| (r.y_min, r.text.clone())).collect();
    v.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    v
}

/// ADR-0009 D3：段落合并——相邻行 y 间距 < 行高 1.5x → 同段。
///
/// 对齐 MinerU `_merge_para_text`：行间无空行（间距小）则合并为一段。
/// 行高用块内中位 region 高度估计；空行/标题行不参与合并。
fn merge_into_paragraphs(lines: &[(f32, String)]) -> Vec<String> {
    if lines.is_empty() {
        return Vec::new();
    }
    // 行高估计：中位 region 高度（无 height 字段，用相邻行 y 差近似）
    let mut gaps: Vec<f32> = Vec::new();
    for w in lines.windows(2) {
        gaps.push((w[1].0 - w[0].0).abs());
    }
    gaps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_gap = gaps.get(gaps.len() / 2).copied().unwrap_or(20.0).max(8.0);
    let merge_threshold = median_gap * 1.5;

    let mut out: Vec<String> = Vec::new();
    let mut cur = lines[0].1.clone();
    for w in lines.windows(2) {
        let gap = (w[1].0 - w[0].0).abs();
        let next_is_heading = w[1].1.starts_with('#');
        let cur_is_heading = w[0].1.starts_with('#');
        // 标题行强制独段；间距超阈值则分段
        if cur_is_heading || next_is_heading || gap > merge_threshold {
            out.push(std::mem::take(&mut cur));
            cur = w[1].1.clone();
        } else {
            // 同段：行间无空行合并（MinerU _merge_para_text 对齐）
            // 不加空格——中文行末无空格，英文连字符已在 postprocess_lines 处理
            cur.push_str(&w[1].1);
        }
    }
    out.push(cur);
    out
}

/// 归一化判定：text 尺度点 `(cx, cy)` 是否落在 layout 尺度 bbox `lb` 内。
/// 各自除以页内最大值转 [0,1]，消除 text/layout 两套坐标尺度差。
fn norm_membership(
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

/// Image 块表格补救：版面模型把超大表格误判为 `Image` 时，用 Image 内文本重建网格。
///
/// 返回 `TableGrid` 仅在以下全部成立（防误判）：
/// - 页面上存在 `LayoutElementType::Image` 块；
/// - Image 块内（中心点在内）text_regions >= 4；
/// - `table_grid::reconstruct_table_grid` 重建出网格（列>=2、行>=2、列 x 对齐）；
/// - 非空单元格占比 >= 50%（真表 vs 散落文本）；
/// - 2 列时 >60% 长文本单元格 → 双列正文 → 拒（与 `is_false_positive_table` 同语义）。
fn reconstruct_image_table(page: &StructureResult, page_w: f32) -> Option<TableGrid> {
    let imgs: Vec<&oar_ocr::domain::structure::LayoutElement> = page
        .layout_elements
        .iter()
        .filter(|el| el.element_type == LayoutElementType::Image)
        .collect();
    if imgs.is_empty() {
        return None;
    }
    // 区分"示意图"与"被误判为表的超大表格"：layout 有 FigureTitle（图题）且无
    // TableTitle → 是图形（如 ISO 9001 图1 过程方法图）→ 跳过重建；有 TableTitle
    // → 是真表（layout 误判 Image，如 C.1）→ 重建。
    let has_figure = page
        .layout_elements
        .iter()
        .any(|el| el.element_type == LayoutElementType::FigureTitle);
    let has_table_title = page
        .layout_elements
        .iter()
        .any(|el| el.element_type == LayoutElementType::TableTitle);
    if has_figure && !has_table_title {
        return None;
    }
    // T02：page_scale 每函数算 1 次（原在 region 循环内每 region 重算，O(n²)）
    let scale = page_scale(page);
    let mut blocks: Vec<Region> = Vec::new();
    if let Some(regs) = &page.text_regions {
        for r in regs {
            let Some(t) = r.text.as_ref() else { continue };
            let t = t.trim();
            if t.is_empty() {
                continue;
            }
            let b = &r.bounding_box;
            let cx = (b.x_min() + b.x_max()) / 2.0;
            let cy = (b.y_min() + b.y_max()) / 2.0;
            let in_img = imgs
                .iter()
                .any(|el| norm_membership(cx, cy, scale, &el.bbox));
            if !in_img {
                continue;
            }
            blocks.push(Region::from_top_left(
                b.x_min(),
                b.y_min(),
                (b.x_max() - b.x_min()).max(1.0),
                (b.y_max() - b.y_min()).max(1.0),
                t.to_string(),
            ));
        }
    }
    if blocks.len() < 4 {
        return None;
    }
    let grid = table_grid::reconstruct_table_grid_tolerant(&blocks, page_w)?;
    // 非空单元格占比：真表单元格大多有内容；散落文本/稀疏网格占比低。
    let mut non_empty = 0usize;
    let mut all = 0usize;
    for c in grid.header.iter().chain(grid.rows.iter().flatten()) {
        all += 1;
        if !c.text.is_empty() {
            non_empty += 1;
        }
    }
    if all == 0 || non_empty * 100 < all * 50 {
        return None;
    }
    // 2 列长文本（对齐双列正文）→ 拒，与 is_false_positive_table 语义一致。
    let cells: Vec<&str> = grid
        .header
        .iter()
        .chain(grid.rows.iter().flatten())
        .filter_map(|c| {
            let t = c.text.trim();
            (!t.is_empty()).then_some(t)
        })
        .collect();
    if is_two_col_prose_like(grid.cols, &cells) {
        return None;
    }
    Some(grid)
}

/// 多页 StructureResult 转为 GFM 文本。
///
/// 输出按页分段（`page_outs`），跨页 Image 重建表挂起、在首表页段 flush
/// （与文字层表格的段式装配一致，保证阅读顺序）。
pub fn structure_results_to_gfm(pages: &[StructureResult]) -> String {
    let debug = std::env::var("ANYDOC_DEBUG_GFM").is_ok();
    // 跨页 Image 重建表：挂起 (grid, 首表页)，列数一致续接，否则 flush。
    let mut emitter = DocumentEmitter::new(FlushFormat::Gfm);

    for (pi, page) in pages.iter().enumerate() {
        // 仅接受通过伪表格过滤的表格：被拒绝的误判表格既不入 HTML，也不
        // 从文本区域中剔除，其区域照常拼入正文行，避免正文丢失。
        let tables: Vec<&TableResult> = page
            .tables
            .iter()
            .filter(|t| !is_false_positive_table(t))
            .collect();
        let page_w = page
            .text_regions
            .as_ref()
            .map(|rs| {
                rs.iter()
                    .map(|r| r.bounding_box.x_max())
                    .fold(0.0_f32, f32::max)
            })
            .unwrap_or(0.0);
        // T02：page_scale 每页算 1 次（原在 region 循环内每 region 重算，O(n²)）
        let scale = page_scale(page);
        // Image 块补救重建（可能跨页续接合并）。重建成功 → Image 内文本从正文
        // 剔除（由跨页表独占，避免表头/单元格正文重复）；失败 → 保留作普通正文。
        let img_grid = reconstruct_image_table(page, page_w);
        let img_bboxes: Vec<&oar_ocr::processors::BoundingBox> = if img_grid.is_some() {
            page.layout_elements
                .iter()
                .filter(|el| el.element_type == LayoutElementType::Image)
                .map(|el| &el.bbox)
                .collect()
        } else {
            Vec::new()
        };

        // 收集文本区域（剔除落在 layout 表格内的，避免与表格 HTML 重复；
        // Image 块文本保留在正文中——重建失败时它应正常输出，重建成功时由
        // 跨页表覆盖首表页段，不再作为正文重复）
        let mut regions: Vec<Region> = Vec::new();
        if let Some(regs) = &page.text_regions {
            for r in regs {
                let Some(t) = r.text.as_ref() else { continue };
                let t = t.trim();
                if t.is_empty() {
                    continue;
                }
                let b = &r.bounding_box;
                let cx = (b.x_min() + b.x_max()) / 2.0;
                let cy = (b.y_min() + b.y_max()) / 2.0;
                let in_table = tables
                    .iter()
                    .any(|tb| norm_membership(cx, cy, scale, &tb.bbox));
                if in_table {
                    continue;
                }
                // Image 重建表：中心点落在任一 Image bbox 内的文本剔除（表 HTML 独占）
                let in_img = img_bboxes
                    .iter()
                    .any(|ib| norm_membership(cx, cy, scale, ib));
                if in_img {
                    continue;
                }
                regions.push(Region::new(
                    b.x_min(),
                    b.x_max(),
                    b.y_min(),
                    b.y_max(),
                    t.to_string(),
                ));
            }
        }
        if debug && pi < 7 {
            let pw = regions.iter().map(|r| r.x_max).fold(0.0_f32, f32::max);
            eprintln!(
                "[gfm-dbg] page={pi} page_w={pw:.0} n_regions={}",
                regions.len()
            );
            for r in &regions {
                let cx = (r.x_min + r.x_max) / 2.0;
                let wide = (r.x_max - r.x_min) > 0.6 * pw;
                eprintln!(
                    "[gfm-dbg]   x0={:6.0} x1={:6.0} cx={cx:6.0} y0={:6.0} y1={:6.0} wide={wide} | {}",
                    r.x_min, r.x_max, r.y_min, r.y_max, r.text
                );
            }
        }
        // 本页正文行 + layout 表格 HTML。对齐 GFM 块语义：标题（# 开头）
        // 与表格前后空行，正文行段落内单换行。
        let mut seg = String::new();
        // ADR-0009：块驱动阅读序 + 段落合并（已合并），postprocess 做连字符/全角归一
        let lines = postprocess_lines(block_driven_order(page, &regions));
        for t in apply_title_prefixes(lines, page) {
            let is_heading = t.starts_with('#');
            if is_heading && !seg.is_empty() && !seg.ends_with("\n\n") {
                seg.push('\n');
            }
            seg.push_str(&t);
            seg.push('\n');
            if is_heading {
                seg.push('\n');
            }
        }
        for table in &tables {
            if let Some(html) = &table.html_structure {
                if !seg.ends_with("\n\n") {
                    seg.push_str("\n\n");
                }
                seg.push_str(&simplify_table_html(html));
            }
        }

        // Image 跨页表处理：同列续接 / 换表 flush / 表格中断 flush
        match img_grid {
            Some(g) => emitter.emit_grid(g, pi as u32),
            None => emitter.flush_pending(),
        }
        emitter.push_segment(pi as u32, &seg);
    }
    emitter.flush_pending();
    emitter.finish()
}

/// 依据版面模型（PP-DocLayout）的 title 块为输出行添加 markdown 标题前缀。
///
/// MinerU 式标题检测：仅对 `LayoutElementType::is_title()`（DocTitle/ParagraphTitle）
/// 的块加前缀；级别来自编号启发式 `title_level`，无编号的短标题（<=40 字符、
/// 不以 。，；： 结尾）回落为 `##`。匹配规则：输出行 trim 后与标题文本相等
/// 或一方包含另一方；已带 `#` 前缀的行跳过，防双重标记。
fn apply_title_prefixes(lines: Vec<String>, page: &StructureResult) -> Vec<String> {
    let mut titles: Vec<(String, usize)> = Vec::new();
    for el in &page.layout_elements {
        if !el.element_type.is_title() {
            continue;
        }
        let Some(t) = el.text.as_ref() else { continue };
        let t = t.trim();
        if t.is_empty() {
            continue;
        }
        // 编号启发式；无编号且像标题（短、无句末标点）→ 2
        let level = title_level(t).or_else(|| {
            let n = t.chars().count();
            (n > 0 && n <= 40 && !t.ends_with(['。', '，', '；', '：'])).then_some(2)
        });
        if let Some(lv) = level {
            titles.push((t.to_string(), lv));
        }
    }
    // 布局驱动：hints 来自版面标题块，numbering=false 不抹平文字层差异。
    crate::text_health::apply_title_prefixes(&lines, &titles, false)
}

/// 剥离 oar-ocr 表格 HTML 的 `<html>/<body>` 包裹（若有），仅保留 `<table>…</table>`。
///
/// 兼容闭合标签缺失 `>` 的畸形输出：实测 oar-ocr 的 html_structure 闭合有时是
/// `</table`（无 `>`）后直接跟同页正文，`rfind("</table>")` 找不到会返回整个
/// html（表格后正文混入）。这里用 `rfind("</table")`（不带 `>`）定位闭合，
/// 截取到闭合标签末尾并补齐缺失的 `>`；表格后的正文由 lines 路径输出，此处丢弃。
fn simplify_table_html(html: &str) -> String {
    let h = html.trim();
    if let Some(s) = h.find("<table")
        && let Some(rel) = h[s..].rfind("</table")
    {
        let mut end = s + rel + "</table".len();
        if h.as_bytes().get(end) == Some(&b'>') {
            end += 1;
        }
        let mut out = h[s..end].to_string();
        if !out.ends_with('>') {
            out.push('>');
        }
        return out;
    }
    h.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use oar_ocr::domain::TextRegion;
    use oar_ocr::domain::structure::{LayoutElement, TableCell, TableType};
    use oar_ocr::processors::BoundingBox;

    fn cell(row: usize, col: usize, text: &str) -> TableCell {
        TableCell::new(BoundingBox::from_coords(0.0, 0.0, 10.0, 10.0), 1.0)
            .with_position(row, col)
            .with_text(text)
    }

    fn table(cells: Vec<TableCell>) -> TableResult {
        TableResult::new(
            BoundingBox::from_coords(0.0, 0.0, 100.0, 100.0),
            TableType::Wireless,
        )
        .with_cells(cells)
    }

    fn tr(x0: f32, y0: f32, x1: f32, y1: f32, text: &str) -> TextRegion {
        TextRegion {
            bounding_box: BoundingBox::from_coords(x0, y0, x1, y1),
            text: Some(text.into()),
            ..TextRegion::new(BoundingBox::from_coords(x0, y0, x1, y1))
        }
    }

    fn image_el(x0: f32, y0: f32, x1: f32, y1: f32) -> LayoutElement {
        LayoutElement::new(
            BoundingBox::from_coords(x0, y0, x1, y1),
            LayoutElementType::Image,
            0.9,
        )
    }

    /// 构造带 order_index 的版面块（ADR-0009 测试用）。
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
                    Region::new(b.x_min(), b.x_max(), b.y_min(), b.y_max(), t.as_ref().to_string())
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
        let out = block_driven_order(&page, &regions);
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
        let out = block_driven_order(&page, &regions);
        // y 排序：上 先于 下
        assert!(out[0].contains("上"));
        assert!(out[1].contains("下"));
    }

    /// ADR-0009 D3：块内段落合并——相邻行 y 间距小 → 合并为一段。
    #[test]
    fn merge_into_paragraphs_joins_close_lines() {
        // y=100,110,120 间距 10（小）→ 合一段；y=200 间距 80（大）→ 分段
        let lines = vec![
            (100.0, "第一行".into()),
            (110.0, "第二行".into()),
            (120.0, "第三行".into()),
            (200.0, "第二段".into()),
        ];
        let out = merge_into_paragraphs(&lines);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], "第一行第二行第三行");
        assert_eq!(out[1], "第二段");
    }

    /// ADR-0009 D3：标题行（# 开头）强制独段，不与相邻行合并。
    #[test]
    fn merge_into_paragraphs_heading_standalone() {
        let lines = vec![
            (100.0, "# 标题".into()),
            (110.0, "正文".into()),
        ];
        let out = merge_into_paragraphs(&lines);
        assert_eq!(out, vec!["# 标题", "正文"]);
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

    /// 构造带 Image 块 + Image 内 2 列网格文本的页（表头 + 数据行）。
    fn page_with_image_grid(
        rows: &[(&str, &str)],
        img_bb: (f32, f32, f32, f32),
    ) -> StructureResult {
        let mut trs = Vec::new();
        // 表头行 y=10
        trs.push(tr(5.0, 10.0, 15.0, 15.0, "编号"));
        trs.push(tr(20.0, 10.0, 40.0, 15.0, "名称"));
        for (i, (a, b)) in rows.iter().enumerate() {
            let y = 20.0 + i as f32 * 10.0;
            trs.push(tr(5.0, y, 15.0, y + 5.0, a));
            trs.push(tr(20.0, y, 40.0, y + 5.0, b));
        }
        StructureResult {
            layout_elements: vec![image_el(img_bb.0, img_bb.1, img_bb.2, img_bb.3)],
            text_regions: Some(trs),
            tables: Vec::new(),
            ..StructureResult::new("t", 0)
        }
    }

    /// Image 块内多列对齐短字段网格 → 重建成功。
    #[test]
    fn image_block_grid_reconstructed() {
        let page = page_with_image_grid(
            &[("1", "甲"), ("2", "乙"), ("3", "丙")],
            (0.0, 0.0, 50.0, 60.0),
        );
        let g = reconstruct_image_table(&page, 100.0).expect("grid");
        assert_eq!(g.cols, 2);
        assert_eq!(g.rows.len(), 3);
    }

    /// Image 块带图题（FigureTitle）无表题 → 示意图 → 跳过重建。
    #[test]
    fn image_block_with_figure_title_skipped() {
        let page = StructureResult {
            layout_elements: vec![
                image_el(0.0, 0.0, 50.0, 60.0),
                LayoutElement::new(
                    BoundingBox::from_coords(0.0, 0.0, 20.0, 10.0),
                    LayoutElementType::FigureTitle,
                    0.9,
                ),
            ],
            text_regions: Some(vec![
                tr(5.0, 10.0, 15.0, 15.0, "编号"),
                tr(20.0, 10.0, 40.0, 15.0, "名称"),
                tr(5.0, 20.0, 15.0, 25.0, "1"),
                tr(20.0, 20.0, 40.0, 25.0, "甲"),
            ]),
            tables: Vec::new(),
            ..StructureResult::new("t", 0)
        };
        assert!(reconstruct_image_table(&page, 100.0).is_none());
    }

    /// Image 块带表题（TableTitle）无图题 → 真表误判 → 重建。
    #[test]
    fn image_block_with_table_title_rebuilt() {
        let page = StructureResult {
            layout_elements: vec![
                image_el(0.0, 0.0, 50.0, 60.0),
                LayoutElement::new(
                    BoundingBox::from_coords(0.0, 0.0, 20.0, 10.0),
                    LayoutElementType::TableTitle,
                    0.9,
                ),
            ],
            text_regions: Some(vec![
                tr(5.0, 10.0, 15.0, 15.0, "编号"),
                tr(20.0, 10.0, 40.0, 15.0, "名称"),
                tr(5.0, 20.0, 15.0, 25.0, "1"),
                tr(20.0, 20.0, 40.0, 25.0, "甲"),
            ]),
            tables: Vec::new(),
            ..StructureResult::new("t", 0)
        };
        assert!(reconstruct_image_table(&page, 100.0).is_some());
    }

    /// Image 块内无文本（真图片）→ None。
    #[test]
    fn image_block_without_text_none() {
        let page = StructureResult {
            layout_elements: vec![image_el(0.0, 0.0, 100.0, 100.0)],
            text_regions: Some(vec![]),
            tables: Vec::new(),
            ..StructureResult::new("t", 0)
        };
        assert!(reconstruct_image_table(&page, 200.0).is_none());
    }

    /// Image 块内 2 列长文本（对齐双列正文）→ 拒。
    #[test]
    fn image_block_two_col_prose_rejected() {
        let page = StructureResult {
            layout_elements: vec![image_el(0.0, 0.0, 60.0, 60.0)],
            text_regions: Some(vec![
                tr(
                    5.0,
                    10.0,
                    25.0,
                    15.0,
                    "经研究，市人民政府决定对下列规章予以修改和废止。",
                ),
                tr(
                    30.0,
                    10.0,
                    55.0,
                    15.0,
                    "受市生态环境部门委托，负责放射源销售单位许可。",
                ),
                tr(
                    5.0,
                    20.0,
                    25.0,
                    25.0,
                    "一、对下列政府规章的部分条款予以修改，现予公布。",
                ),
                tr(
                    30.0,
                    20.0,
                    55.0,
                    25.0,
                    "修改为：市生态环境部门对本市范围内放射性同位素监管。",
                ),
            ]),
            tables: Vec::new(),
            ..StructureResult::new("t", 0)
        };
        assert!(reconstruct_image_table(&page, 100.0).is_none());
    }

    /// 跨页 Image 表合并：两页同列数、下页首行==表头 → 去重合并为 1 个 <table>。
    #[test]
    fn image_table_cross_page_merge() {
        let p1 = page_with_image_grid(&[("1", "甲"), ("2", "乙")], (0.0, 0.0, 50.0, 50.0));
        // 页2：page_with_image_grid 自动生成重复表头 + 续行
        let p2 = page_with_image_grid(&[("3", "丙")], (0.0, 0.0, 50.0, 40.0));
        let out = structure_results_to_gfm(&[p1, p2]);
        assert_eq!(out.matches("<table>").count(), 1, "跨页合并为 1 表");
        assert!(out.contains("丙"), "续行在");
        assert_eq!(out.matches("编号").count(), 1, "表头去重（仅 1 次表头）");
    }

    /// 双栏正文风格的长文本单元格（≥15 字符）超过 60% → 伪表格，拒绝。
    #[test]
    fn two_col_long_prose_is_false_positive() {
        let t = table(vec![
            cell(0, 0, "第一条　为了加强环境保护工作，防止环境污染"),
            cell(0, 1, "第二条　本条例适用于中华人民共和国领域。"),
            cell(1, 0, "第三条　任何单位和个人都有保护环境的义务"),
            cell(1, 1, "第四条　各级人民政府应当加强对环保的领导"),
        ]);
        assert!(is_false_positive_table(&t));
    }

    /// 以句末标点结尾的 2 列单元格同样判为长文本 → 拒绝。
    #[test]
    fn two_col_ending_punct_is_false_positive() {
        let t = table(vec![
            cell(0, 0, "小标题。"),
            cell(0, 1, "正文内容，"),
            cell(1, 0, "短句；"),
            cell(1, 1, "另一句："),
        ]);
        assert!(is_false_positive_table(&t));
    }

    /// 2 列短字段真表格（数字/短语，无长文本）→ 接受。
    #[test]
    fn small_real_table_accepted() {
        let t = table(vec![
            cell(0, 0, "姓名"),
            cell(0, 1, "单位"),
            cell(1, 0, "张三"),
            cell(1, 1, "环保局"),
            cell(2, 0, "李四"),
            cell(2, 1, "水利局"),
        ]);
        assert!(!is_false_positive_table(&t));
    }

    /// 3 列短字段表格 → 接受。
    #[test]
    fn three_col_table_accepted() {
        let t = table(vec![
            cell(0, 0, "a"),
            cell(0, 1, "b"),
            cell(0, 2, "c"),
            cell(1, 0, "1"),
            cell(1, 1, "2"),
            cell(1, 2, "3"),
        ]);
        assert!(!is_false_positive_table(&t));
    }

    /// 单列 → 拒绝。
    #[test]
    fn single_col_rejected() {
        let t = table(vec![cell(0, 0, "一"), cell(1, 0, "二"), cell(2, 0, "三")]);
        assert!(is_false_positive_table(&t));
    }

    /// 单行 → 拒绝。
    #[test]
    fn single_row_rejected() {
        let t = table(vec![cell(0, 0, "a"), cell(0, 1, "b"), cell(0, 2, "c")]);
        assert!(is_false_positive_table(&t));
    }

    /// 无 cells 且无 html_structure → 拒绝。
    #[test]
    fn empty_cells_rejected() {
        let t = TableResult::new(
            BoundingBox::from_coords(0.0, 0.0, 100.0, 100.0),
            TableType::Unknown,
        );
        assert!(is_false_positive_table(&t));
    }
}
