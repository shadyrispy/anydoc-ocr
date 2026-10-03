//! StructureResult → DocIR（图片型 PDF/OFD 的 OCR 结果，P1.5 后产 IR 不再直接产 GFM）
//!
//! 主路径直接读取 OCR 的 `text_regions`（按阅读顺序拼接），而非依赖
//! `StructureResult::to_markdown()`。原因：版面模型（PP-DocLayout）常把
//! 整页图片型文档误判为 `Header`/`Footer`，而 `to_markdown()` 会跳过这些
//! 类型，导致正文丢失。文本区域是 OCR 的直接、可靠结果，不受版面语义分类影响。
//! 表格区域单独用 `html_structure` 输出，并剔除落在表格内的文本区域以防重复。
//!
//! 阅读顺序由公共模块 `crate::reading_order` 还原（双列感知），与文字层通路共用。
//!
//! P1.5：本模块是 OCR 源的 **producer**——`StructureResult` → [`crate::docir::DocIR`]
//! （source=`Ocr` 的页：正文行 Body / 识别表 TableHtml / Image 补救网格 Grid），
//! 渲染由 `docir` 统一消费（AC-6），跨页 Grid 合并由
//! `docir::passes::cross_page_table` 承担（AC-7）。
//!
//! ## Image 块表格补救（A'）
//! 版面模型对"超大表格"（接近整页高、密集多列，如 GJB 标准的附录表）会误判为
//! `Image`（figure）而非 `Table`，导致 `page.tables` 为空、不出 `<table>`。
//! 补救：收集 Image 块内的 text_regions → 网格重建（复用 `crate::table_grid`），
//! 跨页续接合并。防误判见 `reconstruct_image_table`。
use crate::docir::{DocIR, PageSource};
use crate::reading_order::{
    is_index_entry, is_isolated_marker, norm_membership, norm_membership_union,
    order_structure_boxed, page_scale, postprocess_lines_boxed, title_level,
};
use crate::region::{NoiseKind, Region, RegionKind, Span};
use crate::table_grid::{self, TableGrid};
use oar_ocr::domain::structure::{LayoutElementType, StructureResult, TableResult};

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

/// ADR-0009 D1：块驱动阅读序（已迁至 `crate::reading_order::order_structure`）。
/// 本模块仅保留块到表格/标题的装配语义，阅读序收敛于 reading_order.rs。

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

/// 表内嵌图（table-with-image）：表格 bbox 内部含 `Image` 元素时走**本仓网格重建**，
/// 绕开上游被内嵌图污染的 `html_structure`。
///
/// 为什么要重建（synth_samples.pdf 第 2 页取证）：表格里嵌一张图时，上游结构
/// 模型把图所在的那一列**并进了相邻列**——实测 `cells` 里 (0,2)/(1,2)… 的
/// `x_max` 全部越到 x≈730，而表格真实右边界只有 748 内的第 4 列（`趋势`，
/// x≈414-452）。列 2 的 cell 覆盖了列 2 与列 3 两列，于是
/// `split_ocr_box_at_cell_boundaries` 拿 overlapped 的 cell 边界去切跨列 OCR 框：
/// `72.4%`（x=210-263）被 cell(1,0) 的右边界 215.8 切开，按宽度比例 5 字取
/// 1 字 → `7` + `2.4%`；`车载电子`/`83.5%`/`66.4%` 同理被切碎。
///
/// 判据「表内含图」：**文本坐标系**下 table bbox 内部有 `Image` 元素中心点
/// （用 [`norm_membership`]，与 `to_docir` 判定 `in_table` 同一套归一化尺度；
/// layout bbox 与 text bbox 单位不同，见 [`page_scale`]）。
///
/// items 构造：**排除落在内嵌图 bbox 内的文本 region**（图内文字不该进表格；
/// 本页图内零文本，但含图题/轴标签的图会命中此分支）。图内文本同时也不进正文
/// ——`to_docir` 收集 regions 时已按 `in_table` 剔除，本函数只负责表内容。
///
/// 重建失败（列不齐 / 行数不足）返回 `None`，调用方回退 `html_structure`——
/// 宁可输出被切碎的上游表，也不丢表。
fn reconstruct_table_with_embedded_image(
    page: &StructureResult,
    table: &TableResult,
    page_w: f32,
    scale: (f32, f32, f32, f32),
) -> Option<String> {
    // 表内含图？Image 元素中心落在 table bbox 内。
    let imgs: Vec<&oar_ocr::processors::BoundingBox> = page
        .layout_elements
        .iter()
        .filter(|el| el.element_type == LayoutElementType::Image)
        .map(|el| &el.bbox)
        .filter(|ib| {
            let cx = (ib.x_min() + ib.x_max()) / 2.0;
            let cy = (ib.y_min() + ib.y_max()) / 2.0;
            norm_membership(cx, cy, scale, &table.bbox)
        })
        .collect();
    if imgs.is_empty() {
        return None;
    }
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
            // 只取表内文本。
            if !norm_membership(cx, cy, scale, &table.bbox) {
                continue;
            }
            // 图内文字不进表。
            if imgs.iter().any(|ib| norm_membership(cx, cy, scale, ib)) {
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
    // 严格列对齐 + 低分位行距（表内嵌图路径专用）：同列首格 x 散布 ≤ 0.02*page_w。
    // 真实表格因图占位列不齐时返回 None → 上层回退 html_structure。
    let grid = table_grid::reconstruct_table_grid_embedded_image(&blocks, page_w)?;
    // 图占用的列不参与「行内尾空 → colspan」推断（见
    // `table_grid::embedded_image_table_to_html` 的粒度论证）。图 bbox 是 layout
    // 尺度，按 `page_scale` 的同一分母换算到 text 尺度，才能与网格列 x 比较。
    let (tw, _th, lw, _lh) = scale;
    let img_x_ranges: Vec<(f32, f32)> = imgs
        .iter()
        .map(|ib| (ib.x_min() / lw * tw, ib.x_max() / lw * tw))
        .collect();
    Some(table_grid::embedded_image_table_to_html(
        &grid,
        &img_x_ranges,
    ))
}

/// 多页 StructureResult → DocIR（OCR 源 producer，P1.5）。
///
/// 每页产出 source=`Ocr` 的 [`PageIR`]：正文行（阅读顺序 + 标题级别已赋）为
/// `Body` 区块、识别表 HTML 为 `TableHtml` 区块、Image 补救重建网格为 `Grid`
/// 区块。跨页 Grid 合并与 GFM 渲染由调用方经 `DocIR::render()` 统一承担
/// （与文字层表格的段式装配一致，保证阅读顺序）。
///
/// `dims`（#6 第 1 步）：页尺寸，**单位是像素**——OCR 的版面框活在送推理的位图
/// 空间里（见 [`crate::docir::PageDims`] 的单位口径）。按页下标对齐 `pages`；
/// 长度不足或该槽为 `None` 时，该页记 [`PageDims::default`]（`Unknown`），
/// 绝不拿"内容外扩"冒充页面框。
pub fn to_docir(pages: &[StructureResult], dims: &[Option<(u32, u32)>]) -> DocIR {
    let debug = std::env::var("ANYDOC_DEBUG_GFM").is_ok();
    let mut doc = DocIR::default();

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
        // 跨页表覆盖首表页段，不再作为正文重复）。
        // T6：页眉/页脚块（layout 已检出）在 region 收集层剔除——与
        // `order_structure` 的 noise 剔除同一语义，前移做双保险，防路径变化。
        // #10 例外项：**剔除改为分流**——命中文本不再丢弃，收进 `furniture`
        // （kind 按 layout 类型细分，`Footnote` 独立成 kind 不与页脚混），
        // 装配时追加进 IR；渲染默认跳过（输出逐字节不变），开关
        // `ANYDOC_EMIT_FURNITURE` 打开时以注释行输出。正文 regions 仍然
        // 不含家具文本（`order_structure` 的输入与三重过滤语义都不变）。
        let furniture_els: Vec<(&oar_ocr::processors::BoundingBox, RegionKind)> = page
            .layout_elements
            .iter()
            .filter_map(|el| furniture_kind_of(el.element_type).map(|k| (&el.bbox, k)))
            .collect();
        // #10 INDEX（OCR 通路）：版面 Content 块（PP-DocLayout-S 类别 5 "content"，
        // 目录块）。MinerU 口径：`VLM_LAYOUT_LABEL_MAP["content"] → BlockType.INDEX`
        // 对全部档生效（含 basic 的 medium，`_build_vl_style_layout_blocks` 无档位
        // 分支），`PIPELINE_DET_TYPE` 含 index → 块内行照常 OCR、进正文流，仅类型
        // 标 index。本仓同构：行照常参与阅读序/段落合并（点线行由 `is_index_entry`
        // 强制独立），`body_regions_boxed` 之后按几何+形态回贴 [`RegionKind::Index`]。
        // 置信度门槛 0.5 = MinerU PP-DocLayout 同款（`pp_doclayout_v2_base.py:25`）。
        let content_bboxes: Vec<&oar_ocr::processors::BoundingBox> = page
            .layout_elements
            .iter()
            .filter(|el| el.element_type == LayoutElementType::Content && el.confidence >= 0.5)
            .map(|el| &el.bbox)
            .collect();
        // #10 补全：aside/algorithm(→code)/reference/chart 版面元素框，装配后回贴
        // kind（`mark_layout_kinds`）。这些行**不进 furniture**（aside/reference
        // 的 markdown 形态是正文流普通段落，code 是原位 fenced block，chart 是
        // 注释占位）。
        let kind_bboxes: Vec<(&oar_ocr::processors::BoundingBox, RegionKind)> = page
            .layout_elements
            .iter()
            .filter_map(|el| match el.element_type {
                LayoutElementType::AsideText => Some((&el.bbox, RegionKind::Aside)),
                LayoutElementType::Algorithm => Some((&el.bbox, RegionKind::Code)),
                LayoutElementType::Reference | LayoutElementType::ReferenceContent => {
                    Some((&el.bbox, RegionKind::Reference))
                }
                // #10 chart 票：块内文字（图内轴标签/图例/数据标签）回贴 Chart，
                // 渲染层聚合成一个 `<!-- chart -->` 注释占位、不进正文流。
                // 图注**不靠这里**——图注是独立的 `FigureTitle`/`ChartTitle`
                // 元素，照常走普通正文流（见 `RegionKind::Chart` 文档的取证）。
                LayoutElementType::Chart => Some((&el.bbox, RegionKind::Chart)),
                _ => None,
            })
            .collect();
        // #10 切片 5：**目录块确证**（几何先行）。MinerU 的 INDEX 块可以含不带
        // 点线的条目（原文点线在 OCR 阶段被吃掉：实测 nuaa_tupian.pdf 目录页
        // 43 行里 `1 范围1` `7 支持5` `7.2 能力7` 等 20+ 行无点线）→ 纯文本
        // `is_index_entry` 判据接不住，整页并成一坨（本仓 3 条 vs MinerU 85 条）。
        // 版面几何不受 OCR 丢字影响：Content 块内**只要有一行**命中点线形态，
        // 就确证该块是目录块，块内全部行（含丢点线的）逐条独立 + 回贴 Index。
        // 误检护栏：PP-DocLayout-S 会把满页密集正文误检为 content（#10 切片 3
        // 实测），那种块内没有任何点线行 → 不确证 → 合并行为不变（golden 零漂）。
        let mut index_blocks: Vec<bool> = vec![false; content_bboxes.len()];
        if let Some(regs) = &page.text_regions {
            for r in regs {
                let Some(t) = r.text.as_ref() else { continue };
                let t = t.trim();
                if t.is_empty() || !is_index_entry(t) {
                    continue;
                }
                let b = &r.bounding_box;
                let cx = (b.x_min() + b.x_max()) / 2.0;
                let cy = (b.y_min() + b.y_max()) / 2.0;
                for (i, cb) in content_bboxes.iter().enumerate() {
                    if norm_membership_union(cx, cy, scale, cb) {
                        index_blocks[i] = true;
                    }
                }
            }
        }
        let in_index_block = |cx: f32, cy: f32| {
            content_bboxes
                .iter()
                .enumerate()
                .any(|(i, cb)| index_blocks[i] && norm_membership_union(cx, cy, scale, cb))
        };
        let mut furniture: Vec<Region> = Vec::new();
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
                // #10 切片 5：目录块成员 → 逐条独立 + INDEX 回贴（见上）。
                let idx_member = in_index_block(cx, cy);
                // 页眉/页脚/页码/印章/脚注（layout 已检出）：分流进 furniture
                if let Some((_, k)) = furniture_els
                    .iter()
                    .find(|(nb, _)| norm_membership(cx, cy, scale, nb))
                {
                    furniture.push(
                        Region::new(b.x_min(), b.x_max(), b.y_min(), b.y_max(), t.to_string())
                            .with_confidence(r.confidence)
                            .with_kind(k.clone()),
                    );
                    continue;
                }
                // #6 第 4 步：OCR 通路无样式证据（StructureResult 只有整行文本
                // 与置信度），行产**单 span、全零样式**的退化形态——run 边界
                // （行级）结构化，投影层（#11）至少有"整行一个 span"可落。
                regions.push(
                    Region::new(b.x_min(), b.x_max(), b.y_min(), b.y_max(), t.to_string())
                        .with_confidence(r.confidence)
                        .with_spans(vec![Span::plain(t.to_string())])
                        .with_index_member(idx_member),
                );
            }
        }
        if debug && pi < 7 {
            let pw = regions.iter().map(|r| r.x_max).fold(0.0_f32, f32::max);
            eprintln!(
                "[gfm-dbg] page={pi} page_w={pw:.0} n_regions={}",
                regions.len()
            );
            for el in &page.layout_elements {
                if el.element_type.is_header() || el.element_type.is_footer() {
                    eprintln!(
                        "[gfm-dbg]   layout {} x0={:.0} x1={:.0} y0={:.0} y1={:.0}",
                        el.element_type.as_str(),
                        el.bbox.x_min(),
                        el.bbox.x_max(),
                        el.bbox.y_min(),
                        el.bbox.y_max()
                    );
                }
            }
            for r in &regions {
                let cx = (r.x_min + r.x_max) / 2.0;
                let wide = (r.x_max - r.x_min) > 0.6 * pw;
                eprintln!(
                    "[gfm-dbg]   x0={:6.0} x1={:6.0} cx={cx:6.0} y0={:6.0} y1={:6.0} wide={wide} | {}",
                    r.x_min, r.x_max, r.y_min, r.y_max, r.text
                );
            }
        }
        // 本页正文行（标题级别已赋，`#` 前缀由 docir 渲染层写出并据此施加空行
        // 语义；#6 第 2 步）+ layout 表格 HTML。ADR-0009：块驱动阅读序 + 段落
        // 合并，postprocess 做连字符/全角归一，最后依据版面 title 块赋 markdown
        // 标题级别。
        // T6：列表项配对重组（OCR 通路）——det 常把 `b)` 拆成孤立前缀行 + 内容
        // 游离行，此处把孤立 marker 与下一内容行合并为一项（与 a) 形态一致）。
        // T6-②：配对前剔除孤立 ≤1 字符噪声碎片（`馆`），防 marker 误配对。
        let mut out: Vec<Region> = {
            // #11b：走 **boxed** 链路（order → postprocess → body_regions），几何
            // 一路带到 Region 上 → content_list v2 才有 bbox 可投影。文本判定
            // （标题级别）仍按纯文本跑，与改造前同口径。
            let lines = postprocess_lines_boxed(order_structure_boxed(page, &regions));
            let texts: Vec<String> = lines.iter().map(|l| l.text.clone()).collect();
            // hints 对**未加前缀**的行匹配（与旧通路同口径），赋级别不写字面量。
            let layout_on = std::env::var("ANYDOC_HEADINGS_LAYOUT").is_ok();
            let titles = title_hints(page, layout_on);
            let levels = crate::text_health::title_levels(&texts, &titles, false);
            crate::text_health::body_regions_boxed(lines, levels)
        };
        let kept: Vec<Region> = out
            .into_iter()
            .filter(|r| !is_noise_fragment(r))
            .collect();
        out = merge_isolated_markers(kept);
        // #10 补全：aside/algorithm/reference 回贴**先于** INDEX（后者只动
        // Body，先回贴的 kind 不会被点线形态判定覆盖）。
        out = mark_layout_kinds(out, &kind_bboxes, scale);
        // #10 INDEX（OCR 通路）：marker 合并后再回贴——合并不改变行归属，且此时
        // 区域几何/文本已定型。
        out = mark_layout_index(out, &content_bboxes, scale);
        for table in &tables {
            // 表内嵌图 → 本仓网格重建（绕开被图污染的上游 html_structure）。
            // 不含图 / 重建失败 → 走下方原 html_structure 路径（逐字节不变）。
            if let Some(html) = reconstruct_table_with_embedded_image(page, table, page_w, scale) {
                let b = &table.bbox;
                out.push(
                    Region::new(b.x_min(), b.x_max(), b.y_min(), b.y_max(), html)
                        .with_kind(RegionKind::TableHtml),
                );
                continue;
            }
            if let Some(html) = &table.html_structure {
                // #11b：表块几何 = 表格元素自带的 `bbox`（`TableResult.bbox`，
                // 原图坐标）。此前恒退化框 → content_list v2 投影只能省略 bbox。
                let b = &table.bbox;
                out.push(
                    Region::new(b.x_min(), b.x_max(), b.y_min(), b.y_max(), simplify_table_html(html))
                        .with_kind(RegionKind::TableHtml),
                );
            }
        }
        // Image 跨页表（Grid）：同列续接 / 换表定格 / 表格中断由 pass 承担
        if let Some(g) = img_grid {
            // #11b：Grid 块的几何 = 触发它的 Image 元素 bbox（重建源）。
            let gb = page
                .layout_elements
                .iter()
                .find(|el| el.element_type == LayoutElementType::Image)
                .map(|el| (el.bbox.x_min(), el.bbox.x_max(), el.bbox.y_min(), el.bbox.y_max()))
                .unwrap_or((0.0, 0.0, 0.0, 0.0));
            out.push(
                Region::new(gb.0, gb.1, gb.2, gb.3, String::new()).with_kind(RegionKind::Grid(g)),
            );
        }
        // 印章识别行（#10b 起**默认开启**，`ANYDOC_NO_SEAL_OCR` 关闭）：`ocr_post::seal_pass` 已把
        // 识别文本写回 Seal 元素的 `LayoutElement.text`（默认路径该字段恒
        // None——上游 stitching 把 Seal 排除在 OCR 匹配外且标记重叠区域已用，
        // 印章文字本就整体丢失，写回不会与正文行重复）。按 y 升序输出，
        // 每枚章一行 `【印章】<文本>`。
        if crate::ocr_post::seal_on() {
            let mut seals: Vec<(f32, (f32, f32, f32, f32), String)> = page
                .layout_elements
                .iter()
                .filter(|el| el.element_type == LayoutElementType::Seal)
                .filter_map(|el| {
                    el.text.as_ref().map(|t| {
                        let b = &el.bbox;
                        (b.y_min(), (b.x_min(), b.x_max(), b.y_min(), b.y_max()), t.clone())
                    })
                })
                .collect();
            seals.sort_by(|a, b| a.0.total_cmp(&b.0));
            for (_, box4, t) in seals {
                // #11b：识别文本写回自该 Seal 元素 → 行几何 = Seal 元素框
                //（此前退化框，content_list v2 只能省略 bbox）。
                out.push(Region::new(
                    box4.0,
                    box4.1,
                    box4.2,
                    box4.3,
                    format!("{}{}", crate::seal::SEAL_TAG, t),
                ));
            }
        }
        let dims = dims
            .get(pi)
            .copied()
            .flatten()
            .map(|(w, h)| crate::docir::PageDims::page_box_px(w, h))
            .unwrap_or_default();
        // 家具/脚注追加在正文与表格之后（收集顺序无阅读序保证——渲染层按
        // y_min 排序输出，投影层按 bbox 自行排序）。
        out.append(&mut furniture);
        doc.push_page(pi as u32, PageSource::Ocr, out, dims);
    }
    doc
}

/// OCR 版面元素类型 → 家具/脚注 kind（#10 例外项）。
///
/// `None` = 该类型不属家具（正常参与正文装配）。对应关系见
/// [`NoiseKind`] 与 [`RegionKind::Footnote`] 文档；`Footnote` 独立成 kind
/// （MinerU 13 项之 `PAGE_FOOTNOTE`，不与页脚混）。
fn furniture_kind_of(ty: LayoutElementType) -> Option<RegionKind> {
    match ty {
        LayoutElementType::Header | LayoutElementType::HeaderImage => {
            Some(RegionKind::Noise(NoiseKind::Header))
        }
        LayoutElementType::Footer | LayoutElementType::FooterImage => {
            Some(RegionKind::Noise(NoiseKind::Footer))
        }
        LayoutElementType::Number => Some(RegionKind::Noise(NoiseKind::PageNumber)),
        LayoutElementType::Seal => Some(RegionKind::Noise(NoiseKind::Seal)),
        LayoutElementType::Footnote => Some(RegionKind::Footnote),
        _ => None,
    }
}

/// #10 补全（2026-10-01）：版面 `AsideText` / `Algorithm` / `Reference`/
/// `ReferenceContent` / `Chart` 元素内的行 → 回贴 [`RegionKind::Aside`] /
/// [`RegionKind::Code`] / [`RegionKind::Reference`] / [`RegionKind::Chart`]。
///
/// 与 [`mark_layout_index`] 同构：只动 `Body`（标题/表格/家具/已回贴的不
/// 覆盖），行框中心点落在版面元素 bbox 内（`norm_membership_union`）即回贴。
/// 调用时机在段落合并/marker 配对之后——此时 Region 的 bbox 是行框或并段
/// 并集框（stitch 快路行则携带块框），中心点仍落在所属版面元素内。
///
/// 四类的 markdown 形态（MinerU `docvortex blocks.py` 实证）：aside/reference
/// 是**无标记普通段落**（渲染与 Body 同道，正文流位置不变）；code 走 fenced
/// block；chart 走 `<!-- chart -->` 注释占位（块内文字丢弃）。content_list
/// v2：aside → `page_aside_text` 独立 item，reference → 相邻聚合
/// `reference_list`，code → `code` item，chart → `chart` item。
fn mark_layout_kinds(
    mut regions: Vec<Region>,
    kind_bboxes: &[(&oar_ocr::processors::BoundingBox, RegionKind)],
    scale: (f32, f32, f32, f32),
) -> Vec<Region> {
    if kind_bboxes.is_empty() {
        return regions;
    }
    for r in regions.iter_mut() {
        if !matches!(r.kind, RegionKind::Body) {
            continue;
        }
        let cx = (r.x_min + r.x_max) / 2.0;
        let cy = (r.y_min + r.y_max) / 2.0;
        if let Some((_, k)) = kind_bboxes
            .iter()
            .find(|(bb, _)| norm_membership_union(cx, cy, scale, bb))
        {
            r.kind = k.clone();
            // 对齐 mark_layout_index：aside/reference/code 都不是标题行
            //（MinerU RefTextBlock/PageAuxTextBlock/CodeBlock 均无级别语义），
            // 形态判据（如 `1. 总则` 被误赋级）不带入新 kind。
            r.heading_level = None;
        }
    }
    regions
}

/// #10 INDEX（OCR 通路）：几何+形态双判据回贴版面 Content 块。
///
/// 正文 Region 中心点落在版面 `Content`（目录块）bbox 内 **且** 行文本过
/// [`is_index_entry`] 点线判据 → `RegionKind::Index` 并清 `heading_level`
/// （MinerU index item 不带级别）。只动 `Body`——表格 HTML、家具、已定级
/// 标题不回贴。无 Content 元素时零成本直返（多数页面常态）。
///
/// 形态判据不可省：multipage.pdf（满页规则文本）实测 PP-DocLayout-S 会把
/// 整页密集文本误检为 content（MinerU 注释「只在大的目录块中出现」是按大
/// 模型口径；S 版小模型误检率更高），纯几何会让整页正文降级成列表。
/// 版面框只做候选区，行文本形态才是确认——漏判退回旧行为（目录行当正文，
/// 文本不丢），误判则正文被破坏，保守取态。
///
/// 切片 5 增补：`index_member`（`to_docir` 里按"Content 块内存在点线行"确证
/// 的目录块成员）与形态判据取**或**。形态判据在 OCR 丢点线时整页失效（本仓
/// 3 条 vs MinerU 85 条），几何确证接住这部分；误检护栏不松动——误检的满页
/// 正文块内没有点线行，不确证，`index_member` 恒 false。
fn mark_layout_index(
    mut regions: Vec<Region>,
    content_bboxes: &[&oar_ocr::processors::BoundingBox],
    scale: (f32, f32, f32, f32),
) -> Vec<Region> {
    if content_bboxes.is_empty() {
        return regions;
    }
    for r in regions.iter_mut() {
        // 切片 5：`index_member`（目录块确证，几何先行）或行自身点线形态。
        // 前者覆盖 OCR 丢点线的条目行（`1 范围1`），后者是文字层/未确证块的
        // 形态兜底——两者是"或"，几何 membership 仍是必要项（下一个 if）。
        if !matches!(r.kind, RegionKind::Body) || !(r.index_member || is_index_entry(&r.text)) {
            continue;
        }
        let cx = (r.x_min + r.x_max) / 2.0;
        let cy = (r.y_min + r.y_max) / 2.0;
        if content_bboxes
            .iter()
            .any(|cb| norm_membership_union(cx, cy, scale, cb))
        {
            r.kind = RegionKind::Index;
            r.heading_level = None;
        }
    }
    regions
}

/// 多页 StructureResult → GFM 文本（OCR 源便捷入口，P1.5）。
///
/// `to_docir` 产 IR → 跨页表合并 pass → 统一渲染。批量 OCR 主路径
/// （`convert_pdf_ocr` / 质量探针 / OFD 整页 OCR）经此获得与旧 emitter
/// 通路字节一致的输出（golden 守护，AC-8）。
///
/// `dims` 语义同 [`to_docir`]（#6 第 1 步；渲染层不消费，故传空数组与传真实
/// 尺寸的输出**逐字节相同**——这条由本模块单测 `dims_do_not_affect_rendered_markdown`
/// 钉住；`docir`/`gfm_adapter` 是 `pub(crate)`，集成测试看不到 IR，故钉在库内）。
pub fn to_markdown(pages: &[StructureResult], dims: &[Option<(u32, u32)>]) -> String {
    let mut doc = to_docir(pages, dims);
    crate::docir::passes::cross_page_table::run(&mut doc);
    // #10 例外项：家具/脚注的可选输出（存在即开，与 ANYDOC_HEADINGS_LAYOUT
    // 同族 env 语义）。默认关 → 与 `doc.render()` 逐字节相同。
    let emit = std::env::var("ANYDOC_EMIT_FURNITURE").is_ok();
    crate::docir::render::render_with_furniture(&doc, emit)
}

/// T6：OCR 通路列表项配对重组——孤立列表前缀行 + 下一内容行 → 合并为一项。
///
/// det 常把 `b)` 拆成孤立前缀窄条 + 内容宽条两个 region，阅读顺序输出为
/// `b)` 行 + 内容行（游离）。此处将孤立 marker（[`is_isolated_marker`]）与
/// 后续第一个有效内容行合并：`b)` + `本部分强调...` → `b) 本部分强调...`，
/// 与 `a) 完整项` 形态一致。
///
/// 配对时**跳过纯数字短行**（页码，如 `52`）——det 常把页码检出为独立窄条，
/// 排在 marker 与内容之间；直接配对会把页码吞进列表项（`c) 52`）。跳过的行
/// 保留输出（置于配对项之前，近似原位置）。
///
/// 不配对情形：下一行是 marker / `#` 标题（避免跨项/跨标题配对）。
/// 仅 OCR 通路消费（`to_docir`）；文字层通路不经此函数（T6 防回归约束）。
///
/// #6 第 2 步起本函数收发 [`Region`]，三处标题判定一律走
/// [`rendered_line`](Region::rendered_line)（渲染视图）——与旧"producer 先写
/// `#` 字面量、这里 `starts_with('#')`"逐条等价。另注意进入配对分支的 `cur`
/// 必满足 `heading_level == None`：`is_isolated_marker` 对渲染视图判定，而
/// `Some(lv)` 的渲染视图必然以 `#` 开头、会被它排除，故 `cur.text` 就是
/// 渲染文本，拼接时无需再处理前缀。
fn merge_isolated_markers(lines: Vec<Region>) -> Vec<Region> {
    let mut out: Vec<Region> = Vec::with_capacity(lines.len());
    let mut iter = lines.into_iter().peekable();
    while let Some(mut cur) = iter.next() {
        if is_isolated_marker(&cur.rendered_line()) {
            let mut skipped: Vec<Region> = Vec::new();
            let mut paired: Option<Region> = None;
            while let Some(next) = iter.peek() {
                let nxt = next.rendered_line();
                let nxt = nxt.trim_start();
                if nxt.starts_with('#') || is_isolated_marker(nxt) {
                    break; // 标题/下一个 marker：不跨过配对
                }
                if is_page_number(nxt) {
                    skipped.push(iter.next().expect("peeked"));
                    continue; // 跳过页码，继续找内容
                }
                paired = Some(iter.next().expect("peeked"));
                break;
            }
            out.extend(skipped);
            if let Some(content) = paired {
                // #11b：配对合并 → 几何取并集（marker 行与内容行合成一项）。
                // 与 `Line::union_bbox` 同口径：**两侧都有几何才并**，任一侧没有
                // 就退化（合并后的行没有可辩护的完整框，不拿半边的冒充）。
                if cur.has_geometry() && content.has_geometry() {
                    cur.x_min = cur.x_min.min(content.x_min);
                    cur.x_max = cur.x_max.max(content.x_max);
                    cur.y_min = cur.y_min.min(content.y_min);
                    cur.y_max = cur.y_max.max(content.y_max);
                } else {
                    cur.x_min = 0.0;
                    cur.x_max = 0.0;
                    cur.y_min = 0.0;
                    cur.y_max = 0.0;
                }
                let content = content.rendered_line();
                cur.text = format!("{} {}", cur.text, content.trim_start());
            }
        }
        out.push(cur);
    }
    out
}

/// 纯数字短行（页码）：trim 后为 1-4 位数字。配对列表项时跳过，避免把页码吞进项。
fn is_page_number(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty() && t.len() <= 4 && t.chars().all(|c| c.is_ascii_digit())
}

/// 孤立噪声碎片（T6-②）：trim 后 ≤1 字符、非 marker、非标题的独立行——
/// 如页眉残片「馆」。layout 漏检的碎字符在[列表配对]前剔除，避免被 marker
/// 误配对（`c) 馆`）。单字符正文行罕见（"注"/"图"等多带标点或上下文），
/// 且 bullet 单字符（`-`/`•`）是 marker 不受影响，误删风险可控。
///
/// #6 第 2 步起收 [`Region`]：标题判定用 [`is_heading_trimmed`](Region::is_heading_trimmed)
/// （渲染视图 + `trim`），与旧"producer 已写 `#` 字面量、这里 `line.trim().starts_with('#')`"
/// 等价——被赋级别的行渲染后必然以 `#` 开头，两版都不会被当成碎片删掉。
fn is_noise_fragment(region: &Region) -> bool {
    if region.is_heading_trimmed() {
        return false;
    }
    let t = region.text.trim();
    if t.is_empty() {
        return false;
    }
    if t.chars().count() > 1 {
        return false;
    }
    !is_isolated_marker(t)
}

/// 从版面 title 块计算 `(标题文本, markdown 级别)` 提示，供
/// [`crate::text_health::title_levels`] 赋级别（#6 第 2 步起只给级别，不写 `#`）。
///
/// `layout_on` 为纯参数而非直接读环境，便于无 `unsafe set_var` 的单测覆盖两条分支：
/// - `false`（默认）：级别 = 编号语义 或 无编号短标题回落 2，逐字节等价旧行为；
/// - `true`：无编号候选额外走行高/缩进 k-means 投票，可拉出 1..=6 级。
fn title_hints(page: &StructureResult, layout_on: bool) -> Vec<(String, usize)> {
    struct Candidate {
        text: String,
        semantic: Option<usize>,
        idx: usize,
        doc_title: bool,
    }
    let mut cands: Vec<Candidate> = Vec::new();
    let mut heights: Vec<(usize, f32)> = Vec::new();
    let mut indents: Vec<(usize, f32)> = Vec::new();
    for (idx, el) in page.layout_elements.iter().enumerate() {
        if !el.element_type.is_title() {
            continue;
        }
        let Some(t) = el.text.as_ref() else { continue };
        let t = t.trim();
        if t.is_empty() {
            continue;
        }
        let semantic = title_level(t);
        // 无编号时用旧的"短标题"判据决定是否算标题；否则维持不检出。
        let fallback_ok = {
            let n = t.chars().count();
            n > 0 && n <= 40 && !t.ends_with(['。', '，', '；', '：'])
        };
        if semantic.is_none() && !fallback_ok {
            continue;
        }
        let doc_title = el.element_type == LayoutElementType::DocTitle;
        // 布局投票样本仅取 ParagraphTitle（对齐上游：其只聚 ParagraphTitle，
        // 编号/无编号都入样本，投票阶段再由语义权重决定编号项级别）。
        // 无编号 DocTitle 是文档主标题，纳入样本会挤占聚类把小节层级带偏，故排除。
        if layout_on && !doc_title {
            let height = ((el.bbox.y_max() - el.bbox.y_min()).max(1.0))
                / el.num_lines.unwrap_or(1).max(1) as f32;
            heights.push((idx, height.max(1.0)));
            indents.push((idx, el.bbox.x_min()));
        }
        cands.push(Candidate { text: t.to_string(), semantic, idx, doc_title });
    }
    let (font_levels, indent_levels) = if layout_on {
        (
            crate::heading_levels::cluster_levels(&heights, true),
            crate::heading_levels::cluster_levels(&indents, false),
        )
    } else {
        (Default::default(), Default::default())
    };

    let mut titles: Vec<(String, usize)> = Vec::new();
    for c in &cands {
        // 编号命中：语义级别（默认路径同源，开关不改）。
        // 无编号 ParagraphTitle：关闭 → 旧的固定 2；开启 → 行高/缩进聚类投票
        //   （vote_level 在布局信号缺失时自动回落 fallback=2，故与关闭态兼容）。
        // 无编号 DocTitle：两分支都是 2（收集处已排除出样本，见上）。
        let level = match c.semantic {
            Some(lv) => lv,
            None if layout_on && !c.doc_title => crate::heading_levels::vote_level(
                None,
                font_levels.get(&c.idx).copied(),
                indent_levels.get(&c.idx).copied(),
                2,
            ),
            None => 2,
        };
        titles.push((c.text.clone(), level));
    }
    titles
}

/// 剥离 oar-ocr 表格 HTML 的 `<html>/<body>` 包裹（若有），仅保留 `<table>…</table>`。
///
/// 兼容闭合标签缺失 `>` 的畸形输出：实测 oar-ocr 的 html_structure 闭合有时是
/// `</table`（无 `>`）后直接跟同页正文，`rfind("</table>")` 找不到会返回整个
/// html（表格后正文混入）。这里用 `rfind("</table")`（不带 `>`）定位闭合，
/// 截取到闭合标签末尾并补齐缺失的 `>`；表格后的正文由 lines 路径输出，此处丢弃。
fn simplify_table_html(html: &str) -> String {
    let h = html.trim();
    let out = if let Some(s) = h.find("<table")
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
        out
    } else {
        h.to_string()
    };
    dedup_cell_formula_text(&out)
}

/// #9 修法 4：表内单元格的"同一内容两份"去重。
///
/// 成因（第 0 步对拍取证）：表格 OCR 装配把**单元格文本**与**公式识别的
/// LaTeX** 都塞进同一格，产出 `X<br/>$X$`（如
/// `\overline{{f(x)}}=x^{2}+1<br/>$\overline{{f(x)}}=x^{2}+1$`）——
/// 同一格内容出现两份，MinerU basic 输出一份。去重取**LaTeX/`$…$` 形态**
/// （信息更全：含公式结构，纯文本那份是 rec 的近似）。
///
/// 判定刻意保守：只在 `</td>` 内的文本**恰好被 `<br/>` 分成两段**、且其中
/// 一段是 `$…$` 包裹、另一段去掉 `$` 与空白后**内容相同**时才合并——
/// 不匹配则原样返回（避免误伤正常的 `<br/>` 多行单元格）。
fn dedup_cell_formula_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    loop {
        let Some(open) = rest.find("<td") else {
            out.push_str(rest);
            return out;
        };
        let Some(gt) = rest[open..].find('>') else {
            out.push_str(rest);
            return out;
        };
        // `<td …>` 头
        let head_end = open + gt + 1;
        let Some(close) = rest[head_end..].find("</td>") else {
            out.push_str(rest);
            return out;
        };
        let cell_end = head_end + close;
        let cell = &rest[head_end..cell_end];
        out.push_str(&rest[..head_end]);
        out.push_str(&dedup_one_cell(cell));
        rest = &rest[cell_end..];
    }
}

/// 单个 `<td>` **内容**的去重（不含标签头尾）。
fn dedup_one_cell(cell: &str) -> String {
    let parts: Vec<&str> = cell.split("<br/>").collect();
    if parts.len() != 2 {
        return cell.to_string();
    }
    let (a, b) = (parts[0].trim(), parts[1].trim());
    // 一段带 `$…$`、另一段剥掉 `$` 后两者相同 → 取带定界符的那份
    if (is_dollar_wrapped(a) || is_dollar_wrapped(b)) && strip_dollars(a) == strip_dollars(b) {
        return if is_dollar_wrapped(a) { a } else { b }.to_string();
    }
    cell.to_string()
}

/// 是否 `$…$` 包裹（长度 > 1，排除单个 `$`）。
fn is_dollar_wrapped(s: &str) -> bool {
    s.len() > 1 && s.starts_with('$') && s.ends_with('$')
}

/// 剥掉首尾空白与 `$` 定界符。
fn strip_dollars(s: &str) -> &str {
    s.trim().trim_matches('$').trim()
}

#[cfg(test)]
mod tests {
    use super::*;
    use oar_ocr::domain::TextRegion;
    use oar_ocr::domain::structure::{LayoutElement, TableCell, TableType};
    use oar_ocr::processors::BoundingBox;

    // ── T6：列表项配对重组 ──
    //
    // #6 第 2 步后这两个函数收发 [`Region`]，故测试用 [`ln`]（正文行）/ [`hd`]
    // （带级别行）构造，断言一律走 `rendered_line`（渲染视图）——与改造前对
    // `Vec<String>` 的断言等价。
    fn ln(text: &str) -> Region {
        Region::new(0.0, 0.0, 0.0, 0.0, text.to_string())
    }
    fn hd(text: &str, level: u8) -> Region {
        ln(text).with_heading_level(Some(level))
    }
    fn rendered(lines: &[Region]) -> Vec<String> {
        lines.iter().map(|r| r.rendered_line().into_owned()).collect()
    }

    #[test]
    fn merge_isolated_marker_with_next_content() {
        // b) 孤立前缀 + 内容行 → 合并为一项（与 a) 形态一致）
        let lines = vec![
            ln("a) 本部分更加强调性能要求"),
            ln("b)"),
            ln("本部分强调规范的内容只包括"),
            ln("c)"),
            ln("本部分将原《规范的编写》移入附录"),
        ];
        let out = merge_isolated_markers(lines);
        assert_eq!(
            rendered(&out),
            vec![
                "a) 本部分更加强调性能要求",
                "b) 本部分强调规范的内容只包括",
                "c) 本部分将原《规范的编写》移入附录",
            ]
        );
    }

    #[test]
    fn merge_keeps_heading_untouched() {
        // 孤立 marker 后是标题行 → 不配对，标题不被吞。
        // 两种形态都要覆盖：IR 赋级别（`#` 由渲染层写出）与来源文本自带 `#` 字面量。
        let by_level = merge_isolated_markers(vec![ln("b)"), hd("4. 总则", 1), ln("正文")]);
        assert_eq!(rendered(&by_level), vec!["b)", "# 4. 总则", "正文"]);
        let by_literal = merge_isolated_markers(vec![ln("b)"), ln("# 4. 总则"), ln("正文")]);
        assert_eq!(rendered(&by_literal), vec!["b)", "# 4. 总则", "正文"]);
    }

    #[test]
    fn merge_no_marker_unchanged() {
        let lines = vec![ln("普通正文一行"), ln("普通正文二行")];
        assert_eq!(rendered(&merge_isolated_markers(lines.clone())), rendered(&lines));
    }

    #[test]
    fn merge_consecutive_markers_not_paired() {
        // 连续 marker：b) 不把 c) 当内容；但 c) 仍与后续内容行配对
        let lines = vec![ln("b)"), ln("c)"), ln("内容")];
        let out = merge_isolated_markers(lines);
        assert_eq!(rendered(&out), vec!["b)", "c) 内容"]);
    }

    #[test]
    fn merge_skips_page_number_before_content() {
        // c) 后是页码 52，再后才是内容 → 跳过页码，配对真正内容；页码保留在配对项前
        let lines = vec![
            ln("b)"),
            ln("本部分强调规范的内容"),
            ln("c)"),
            ln("52"),
            ln("本部分将原《规范的编写》移入附录"),
        ];
        let out = merge_isolated_markers(lines);
        assert_eq!(
            rendered(&out),
            vec![
                "b) 本部分强调规范的内容",
                "52",
                "c) 本部分将原《规范的编写》移入附录",
            ],
            "跳过 52 配对内容，页码保留"
        );
    }

    #[test]
    fn page_number_detector() {
        assert!(is_page_number("52"));
        assert!(is_page_number("  7 "));
        assert!(!is_page_number(""));
        assert!(!is_page_number("52a"));
        assert!(!is_page_number("12345"), ">4 位不算页码");
        assert!(!is_page_number("本部分强调"));
    }

    #[test]
    fn noise_fragment_detector() {
        assert!(is_noise_fragment(&ln("馆")), "单字符噪声残片");
        assert!(is_noise_fragment(&ln(" 馆 ")), "允许首尾空白");
        assert!(!is_noise_fragment(&ln("")), "空行保留");
        assert!(!is_noise_fragment(&ln("a)")), "marker 保留");
        assert!(!is_noise_fragment(&ln("-")), "bullet marker 保留");
        // 标题保留：IR 赋级别（渲染视图带 `#`）与来源字面量两条都要钉
        assert!(!is_noise_fragment(&hd("标题", 2)), "带级别的标题保留");
        assert!(!is_noise_fragment(&ln("# 标题")), "字面量标题保留");
        assert!(!is_noise_fragment(&ln("本部分强调")), "内容保留");
        assert!(!is_noise_fragment(&ln("52")), "数字由 is_page_number 处理");
    }

    #[test]
    fn merge_skips_noise_fragment_then_pairs_content() {
        // 馆（噪声残片）在 c) 与内容之间：merge 前已被过滤 → c) 直接配到内容
        let lines = vec![ln("c)"), ln("馆"), ln("本部分将原《规范的编写》移入附录")];
        let filtered: Vec<Region> = lines
            .into_iter()
            .filter(|r| !is_noise_fragment(r))
            .collect();
        let out = merge_isolated_markers(filtered);
        assert_eq!(rendered(&out), vec!["c) 本部分将原《规范的编写》移入附录"]);
    }

    /// #6 第 2 步：级别在 marker 配对与噪声过滤后必须**留在** Region 上
    /// （旧通路靠字面量携带，新通路靠字段；这两处是唯一的"文本被改写"点，
    /// 级别若在那里丢了，markdown 就再也拼不回 `##`）。
    #[test]
    fn heading_level_survives_marker_merge_and_filter() {
        let lines = vec![hd("1. 总则", 2), ln("b)"), ln("内容行"), ln("馆")];
        let kept: Vec<Region> = lines
            .into_iter()
            .filter(|r| !is_noise_fragment(r))
            .collect();
        let out = merge_isolated_markers(kept);
        assert_eq!(rendered(&out), vec!["## 1. 总则", "b) 内容行"]);
        assert_eq!(out[0].heading_level, Some(2), "级别仍在 IR 上");
        assert_eq!(out[0].text, "1. 总则", "text 不含字面量");
        assert_eq!(out[1].heading_level, None);
    }

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

    /// 构造版面 title 块（含行高特征所需的 num_lines）。
    fn title_el(
        kind: LayoutElementType,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        text: &str,
        num_lines: u32,
    ) -> LayoutElement {
        let mut el = LayoutElement::new(BoundingBox::from_coords(x0, y0, x1, y1), kind, 0.9);
        el.text = Some(text.into());
        el.num_lines = Some(num_lines);
        el
    }

    /// 无编号标题：默认分支全部回落 `##`（字节兼容），布局分支按行高拉开层级。
    #[test]
    fn title_hints_unnumbered_levels() {
        // 四个同级候选分两簇（行高 30 vs 14），避免 2 样本 kmeans 每样本自成一簇
        // 的退化形态——那正是上游算法的行为，测试要覆盖的是"拉开层级"而非退化。
        let page = StructureResult {
            layout_elements: vec![
                title_el(LayoutElementType::ParagraphTitle, 40.0, 0.0, 200.0, 30.0, "总则", 1),
                title_el(LayoutElementType::ParagraphTitle, 40.0, 50.0, 200.0, 80.0, "分类", 1),
                title_el(LayoutElementType::ParagraphTitle, 40.0, 90.0, 200.0, 104.0, "适用范围", 1),
                title_el(LayoutElementType::ParagraphTitle, 120.0, 110.0, 260.0, 124.0, "术语定义", 1),
            ],
            text_regions: None,
            tables: Vec::new(),
            ..StructureResult::new("t", 0)
        };
        // 关闭（默认）：全部是无编号短标题 → 一律 2。
        let off = title_hints(&page, false);
        assert_eq!(
            off,
            vec![
                ("总则".to_string(), 2),
                ("分类".to_string(), 2),
                ("适用范围".to_string(), 2),
                ("术语定义".to_string(), 2),
            ]
        );
        // 开启（两信号联合）：
        // - 总则/分类：font=1；indent 同簇(40)=1 → 一致 → 1 级；
        // - 适用范围：font=2、indent=1 → 平手取小 → 1 级（缩进信号把它拉高，
        //   这是上游三信号投票的既定语义，非本实现缺陷）；
        // - 术语定义：font=2、indent=2 → 一致 → 2 级。
        // 断言重点 = 默认被抹平的标题被拉开，且逐项与投票语义吻合。
        let on = title_hints(&page, true);
        assert_eq!(on[0], ("总则".to_string(), 1), "大字号 → 一级");
        assert_eq!(on[1], ("分类".to_string(), 1));
        assert_eq!(on[2], ("适用范围".to_string(), 1), "font2/indent1 平手取小");
        assert_eq!(on[3], ("术语定义".to_string(), 2), "小字号 + 深缩进 → 二级");
    }

    /// 编号命中的标题在两个分支下级别一致（语义优先，开关不动它）。
    #[test]
    fn title_hints_numbered_stable_across_flag() {
        let page = StructureResult {
            layout_elements: vec![
                title_el(LayoutElementType::DocTitle, 0.0, 0.0, 300.0, 60.0, "GB 3836.1—2021", 1),
                title_el(LayoutElementType::ParagraphTitle, 0.0, 70.0, 200.0, 100.0, "2.1 环境条件", 1),
            ],
            text_regions: None,
            tables: Vec::new(),
            ..StructureResult::new("t", 0)
        };
        assert_eq!(title_hints(&page, false), title_hints(&page, true));
        assert_eq!(title_hints(&page, true)[1], ("2.1 环境条件".to_string(), 3));
    }

    /// 句末标点/超长行不是标题（两分支同判，旧 or_else 语义保持）。
    #[test]
    fn title_hints_rejects_sentence_like_candidates() {
        let page = StructureResult {
            layout_elements: vec![
                title_el(
                    LayoutElementType::ParagraphTitle,
                    0.0,
                    0.0,
                    400.0,
                    20.0,
                    "本部分规定了设备的通用要求。",
                    1,
                ),
                title_el(
                    LayoutElementType::ParagraphTitle,
                    0.0,
                    30.0,
                    900.0,
                    50.0,
                    &"长".repeat(41),
                    1,
                ),
            ],
            text_regions: None,
            tables: Vec::new(),
            ..StructureResult::new("t", 0)
        };
        assert!(title_hints(&page, false).is_empty());
        assert!(title_hints(&page, true).is_empty());
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
        let out = to_markdown(&[p1, p2], &[]);
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

    // ── #6 第 1 步：页尺寸进 IR（写而不消费）──

    /// 单页 OCR 结果（无 layout 块 → 走 `order_structure` 的降级路径）。
    fn ocr_page_1() -> StructureResult {
        StructureResult {
            layout_elements: vec![],
            text_regions: Some(vec![tr(10.0, 10.0, 400.0, 25.0, "正文行")]),
            tables: Vec::new(),
            ..StructureResult::new("t", 0)
        }
    }

    /// **第 1 步零回归的机制性保证**：渲染层不消费 `dims`，故传空数组与传真实
    /// 位图尺寸的 markdown 必须逐字节相同（否则第 1 步就不是"纯 IR 增量"）。
    /// `docir`/`gfm_adapter` 是 `pub(crate)`、集成测试看不到 IR，所以这条钉在库内。
    #[test]
    fn dims_do_not_affect_rendered_markdown() {
        let with = to_markdown(&[ocr_page_1()], &[Some((1240, 1754))]);
        let without = to_markdown(&[ocr_page_1()], &[]);
        assert_eq!(with, without, "dims 不得改变输出的任何一个字节");
    }

    /// OCR producer 把位图尺寸如实写进 IR：`PageBox` + 像素单位（版面框本就活在
    /// 送推理的位图空间里）。
    #[test]
    fn ocr_producer_records_page_box_in_px() {
        let d = to_docir(&[ocr_page_1()], &[Some((1240, 1754))]);
        let dims = &d.pages[0].dims;
        assert_eq!(dims.kind, crate::docir::PageDimsKind::PageBox);
        assert_eq!(dims.unit, crate::docir::PageUnit::Px);
        assert!((dims.w - 1240.0).abs() < 1e-6 && (dims.h - 1754.0).abs() < 1e-6);
        assert!(dims.normalizable(), "OCR 页的位图宽高就是合法归一化分母");
    }

    /// **下标契约**（唯一可能悄悄错配的地方）：`dims` 按 `pages` 的迭代下标对齐，
    /// 两页各给不同尺寸 → 反序/串位会立刻暴露。markdown 看不到 dims，golden
    /// 永远抓不到这类错配，只能靠这条钉住。
    #[test]
    fn dims_align_with_page_index_not_order_of_arrival() {
        // 每次现造（StructureResult 不可复用：to_docir 拿走所有权）
        fn ocr_page_2() -> StructureResult {
            StructureResult {
                layout_elements: vec![],
                text_regions: Some(vec![tr(10.0, 10.0, 400.0, 25.0, "第二页")]),
                tables: Vec::new(),
                ..StructureResult::new("t", 1)
            }
        }
        let d = to_docir(&[ocr_page_1(), ocr_page_2()], &[Some((100, 200)), Some((300, 400))]);
        assert_eq!(d.pages.len(), 2);
        assert!((d.pages[0].dims.w - 100.0).abs() < 1e-6, "页 0 应配 100×200");
        assert!((d.pages[0].dims.h - 200.0).abs() < 1e-6);
        assert!((d.pages[1].dims.w - 300.0).abs() < 1e-6, "页 1 应配 300×400");
        assert!((d.pages[1].dims.h - 400.0).abs() < 1e-6);
        // 只给一页的尺寸：另一页 Unknown，而不是"借用"邻居的。
        let partial = to_docir(&[ocr_page_1(), ocr_page_2()], &[Some((100, 200))]);
        assert_eq!(
            partial.pages[1].dims.kind,
            crate::docir::PageDimsKind::Unknown,
            "缺槽不得顺延邻居的尺寸"
        );
    }

    /// dims 槽位缺失（空数组）与该槽为 `None` 都只能记 `Unknown`——
    /// 绝不允许拿"内容外扩"或别的页的尺寸冒充页面框。
    #[test]
    fn missing_dims_record_unknown_not_a_guess() {
        for case in [&[] as &[Option<(u32, u32)>], &[None], &[None, Some((1, 1))]] {
            let d = to_docir(&[ocr_page_1()], case);
            let dims = &d.pages[0].dims;
            assert_eq!(
                dims.kind,
                crate::docir::PageDimsKind::Unknown,
                "dims={case:?} 时应记 Unknown"
            );
            assert_eq!(dims.unit, crate::docir::PageUnit::Unknown);
            assert!(!dims.normalizable());
            assert_eq!((dims.w, dims.h), (0.0, 0.0));
        }
    }

    // ── #10 INDEX（OCR 通路）：版面 Content 块几何回贴 ──

    /// 版面 Content（目录块）内的行 → `RegionKind::Index`；块外正文行不动。
    /// MinerU basic 口径：PP-DocLayout label "content" → `BlockType.INDEX`
    /// （`VLM_LAYOUT_LABEL_MAP` 全档共用），块内行照常 OCR 进正文流。
    #[test]
    fn layout_content_block_marks_index_entries() {
        fn page() -> StructureResult {
            StructureResult {
                // Content 目录块 + 一个底部 Text 锚点（把 layout 尺度撑到与
                // text 尺度同页——真实情形里版面框覆盖整页，两个坐标系的最大
                // 值接近，归一化才忠实）。
                layout_elements: vec![
                    LayoutElement::new(
                        BoundingBox::from_coords(40.0, 100.0, 400.0, 160.0),
                        LayoutElementType::Content,
                        0.9,
                    ),
                    LayoutElement::new(
                        BoundingBox::from_coords(40.0, 250.0, 400.0, 300.0),
                        LayoutElementType::Text,
                        0.9,
                    ),
                ],
                text_regions: Some(vec![
                    tr(50.0, 105.0, 390.0, 120.0, "前言.......IV"),
                    tr(50.0, 130.0, 390.0, 145.0, "1 范围.......1"),
                    tr(50.0, 250.0, 390.0, 265.0, "正文行在块外"),
                ]),
                tables: Vec::new(),
                ..StructureResult::new("t", 0)
            }
        }
        let doc = to_docir(&[page()], &[]);
        let regions = &doc.pages[0].regions;
        let kinds: Vec<_> = regions.iter().map(|r| (&r.text, &r.kind)).collect();
        let idx: Vec<&Region> = regions
            .iter()
            .filter(|r| matches!(r.kind, RegionKind::Index))
            .collect();
        assert_eq!(
            idx.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(),
            vec!["前言.......IV", "1 范围.......1"],
            "两条目录行都应回贴 Index，实得 {kinds:?}"
        );
        let body: Vec<&Region> = regions
            .iter()
            .filter(|r| matches!(r.kind, RegionKind::Body))
            .collect();
        assert_eq!(body.len(), 1, "块外正文行不受影响，实得 {kinds:?}");
        assert_eq!(body[0].text, "正文行在块外");
        // 渲染 `- ` 条目（GFM 列表语义，与文字层 INDEX 同形态）
        let md = doc.render();
        assert!(md.contains("- 前言.......IV"), "渲染应含条目行：{md:?}");
        assert!(md.contains("- 1 范围.......1"));
    }

    /// 版面无 Content 元素 → 回贴零成本直返，页面输出与改造前一致。
    #[test]
    fn no_content_element_keeps_body_untouched() {
        let d = to_docir(&[ocr_page_1()], &[]);
        assert!(d.pages[0]
            .regions
            .iter()
            .all(|r| matches!(r.kind, RegionKind::Body)));
    }

    /// #10 切片 5：目录块**确证**——Content 块内只要有**一行**含点线，块内丢
    /// 点线的条目行（`1 范围1` `7 支持5`：OCR 把引导点线整段吃掉）也标 Index
    /// 且逐条独立。纯形态判据在这些行上整页失效（实测本仓 3 条 vs MinerU 85 条）。
    #[test]
    fn index_block_confirmation_marks_dotless_entries() {
        let page = StructureResult {
            layout_elements: vec![
                LayoutElement::new(
                    BoundingBox::from_coords(40.0, 100.0, 400.0, 300.0),
                    LayoutElementType::Content,
                    0.9,
                ),
                LayoutElement::new(
                    BoundingBox::from_coords(40.0, 400.0, 400.0, 450.0),
                    LayoutElementType::Text,
                    0.9,
                ),
            ],
            text_regions: Some(vec![
                tr(50.0, 105.0, 390.0, 120.0, "前言.......IV"), // 点线确证行
                tr(50.0, 150.0, 390.0, 165.0, "1 范围1"),       // 丢点线
                tr(50.0, 195.0, 390.0, 210.0, "7 支持5"),       // 丢点线
                tr(50.0, 400.0, 390.0, 415.0, "正文行在块外"),
            ]),
            tables: Vec::new(),
            ..StructureResult::new("t", 0)
        };
        let doc = to_docir(&[page], &[]);
        let idx: Vec<&str> = doc.pages[0]
            .regions
            .iter()
            .filter(|r| matches!(r.kind, RegionKind::Index))
            .map(|r| r.text.as_str())
            .collect();
        assert_eq!(
            idx,
            vec!["前言.......IV", "1 范围1", "7 支持5"],
            "确证后块内条目须全部逐条独立并回贴 Index，块外正文不受影响"
        );
    }

    /// 回贴只动 `Body`：同坐标同文本，Body 升格 Index，表格 HTML / 标题不被覆盖。
    #[test]
    fn layout_index_marking_only_touches_body() {
        let mut table = Region::new(50.0, 390.0, 105.0, 120.0, "前言.......IV".to_string());
        table.kind = RegionKind::TableHtml;
        let mut body = Region::new(50.0, 390.0, 105.0, 120.0, "前言.......IV".to_string());
        body.kind = RegionKind::Body;
        let cb = BoundingBox::from_coords(40.0, 100.0, 400.0, 160.0);
        let out = mark_layout_index(vec![table, body], &[&cb], (390.0, 120.0, 400.0, 160.0));
        assert!(matches!(out[0].kind, RegionKind::TableHtml), "表格 HTML 不回贴");
        assert!(matches!(out[1].kind, RegionKind::Index), "Body 应升格 Index");
    }

    /// 低置信度 Content 误检不回贴。PP-DocLayout-S 在满页规则文本上会把整页
    /// 误检为 content（multipage.pdf 实测），阈值 0.5 对齐 MinerU PP-DocLayout
    /// （`pp_doclayout_v2_base.py:25`）。形态判据（`is_index_entry`）由
    /// `layout_content_block_marks_index_entries` 的块外正文行负例覆盖。
    #[test]
    fn low_confidence_content_not_marked() {
        fn page() -> StructureResult {
            StructureResult {
                layout_elements: vec![
                    LayoutElement::new(
                        BoundingBox::from_coords(40.0, 100.0, 400.0, 160.0),
                        LayoutElementType::Content,
                        0.4,
                    ),
                    LayoutElement::new(
                        BoundingBox::from_coords(40.0, 250.0, 400.0, 300.0),
                        LayoutElementType::Text,
                        0.9,
                    ),
                ],
                text_regions: Some(vec![
                    tr(50.0, 105.0, 390.0, 120.0, "前言.......IV"),
                    tr(50.0, 250.0, 390.0, 265.0, "正文行在块外"),
                ]),
                tables: Vec::new(),
                ..StructureResult::new("t", 0)
            }
        }
        let doc = to_docir(&[page()], &[]);
        assert!(
            doc.pages[0]
                .regions
                .iter()
                .all(|r| matches!(r.kind, RegionKind::Body)),
            "0.4 置信度 Content 不得回贴：{:?}",
            doc.pages[0].regions.iter().map(|r| &r.kind).collect::<Vec<_>>()
        );
    }

    // ── #10 例外项：家具/脚注分流（收集进 IR，渲染默认跳过）──

    /// 版面家具页：Header / Footer / Number / Footnote / Seal 各一块，
    /// 每块 bbox 内一条 OCR 文本 + 页中一条正文。
    fn furniture_page() -> StructureResult {
        StructureResult {
            layout_elements: vec![
                LayoutElement::new(BoundingBox::from_coords(0.0, 0.0, 100.0, 10.0), LayoutElementType::Header, 0.9),
                LayoutElement::new(BoundingBox::from_coords(0.0, 90.0, 100.0, 100.0), LayoutElementType::Footer, 0.9),
                LayoutElement::new(BoundingBox::from_coords(0.0, 80.0, 100.0, 90.0), LayoutElementType::Number, 0.9),
                LayoutElement::new(BoundingBox::from_coords(0.0, 70.0, 100.0, 80.0), LayoutElementType::Footnote, 0.9),
                LayoutElement::new(BoundingBox::from_coords(0.0, 60.0, 100.0, 70.0), LayoutElementType::Seal, 0.9),
            ],
            text_regions: Some(vec![
                tr(10.0, 2.0, 90.0, 8.0, "页眉文本"),
                tr(10.0, 40.0, 90.0, 50.0, "正文一行"),
                tr(10.0, 62.0, 90.0, 68.0, "章内散字"),
                tr(10.0, 72.0, 90.0, 78.0, "脚注一行"),
                tr(10.0, 82.0, 90.0, 88.0, "第 1 页"),
                tr(10.0, 92.0, 90.0, 98.0, "页脚文本"),
            ]),
            tables: Vec::new(),
            ..StructureResult::new("t", 0)
        }
    }

    #[test]
    fn furniture_is_collected_into_ir_with_kinds() {
        let d = to_docir(&[furniture_page()], &[Some((100, 100))]);
        let regs = &d.pages[0].regions;
        // 正文只有页中的一行；五类家具文本不再丢失而是带着 kind 进 IR
        let body: Vec<&str> = regs
            .iter()
            .filter(|r| r.kind == RegionKind::Body)
            .map(|r| r.text.as_str())
            .collect();
        assert_eq!(body, vec!["正文一行"]);
        let kind_of = |t: &str| {
            regs.iter()
                .find(|r| r.text == t)
                .map(|r| r.kind.clone())
                .unwrap_or_else(|| panic!("文本 {t} 应在 IR 中"))
        };
        assert_eq!(kind_of("页眉文本"), RegionKind::Noise(NoiseKind::Header));
        assert_eq!(kind_of("第 1 页"), RegionKind::Noise(NoiseKind::PageNumber));
        assert_eq!(kind_of("页脚文本"), RegionKind::Noise(NoiseKind::Footer));
        assert_eq!(kind_of("章内散字"), RegionKind::Noise(NoiseKind::Seal));
        // 脚注独立成 kind（MinerU 13 项之 PAGE_FOOTNOTE），不与页脚混
        assert_eq!(kind_of("脚注一行"), RegionKind::Footnote);
    }

    #[test]
    fn furniture_regions_keep_bbox_and_confidence() {
        let d = to_docir(&[furniture_page()], &[Some((100, 100))]);
        let hdr = d.pages[0]
            .regions
            .iter()
            .find(|r| r.text == "页眉文本")
            .expect("页眉在 IR");
        // bbox 原样保留（投影层要落 PAGE_HEADER 的 bbox）
        assert!((hdr.x_min - 10.0).abs() < 1e-4 && (hdr.x_max - 90.0).abs() < 1e-4);
        assert!((hdr.y_min - 2.0).abs() < 1e-4 && (hdr.y_max - 8.0).abs() < 1e-4);
    }

    // ── #10 补全：aside/algorithm(→code)/reference 版面元素行回贴 kind ──

    /// `mark_layout_kinds` 直接单测：Body 行中心点命中版面元素框 → 回贴
    /// kind 并清 heading_level；框外行保持 Body；非 Body 不被覆盖。
    /// （不走 to_docir 装配链——稀疏测试布局会被 `merge_into_paragraphs`
    /// 按行距中位数×1.5 全部并段，装配后的 region 几何不可控。）
    #[test]
    fn mark_layout_kinds_marks_matched_bodies_only() {
        // scale = (tw, th, lw, lh)：页面 100×100（`page_scale` 语义）。
        let scale = (100.0_f32, 100.0_f32, 100.0_f32, 100.0_f32);
        // 生产路径收集的是 layout_elements 的 bbox 引用，同构。
        let bb_aside = BoundingBox::from_coords(0.0, 10.0, 100.0, 20.0);
        let bb_code = BoundingBox::from_coords(0.0, 30.0, 100.0, 40.0);
        let bb_ref = BoundingBox::from_coords(0.0, 50.0, 100.0, 100.0);
        let bboxes = vec![
            (&bb_aside, RegionKind::Aside),
            (&bb_code, RegionKind::Code),
            (&bb_ref, RegionKind::Reference),
        ];
        let mut body_in_aside = Region::new(10.0, 90.0, 12.0, 18.0, "旁注一行");
        body_in_aside.heading_level = Some(2);
        // "脚注一行"中心也落在 Aside 框内，但非 Body 不回贴。
        let regions = vec![
            body_in_aside,
            Region::new(10.0, 90.0, 32.0, 38.0, "x = f(a)"),
            Region::new(10.0, 90.0, 52.0, 58.0, "〔1〕文献一"),
            Region::new(10.0, 90.0, 92.0, 98.0, "〔2〕文献二"),
            // 中心 y=7：三个框都罩不住 → 保持 Body。
            Region::new(10.0, 90.0, 5.0, 9.0, "正文一行"),
            Region::new(10.0, 90.0, 12.0, 18.0, "脚注一行").with_kind(RegionKind::Footnote),
        ];
        let out = mark_layout_kinds(regions, &bboxes, scale);
        let kind_of = |t: &str| {
            out.iter()
                .find(|r| r.text == t)
                .map(|r| r.kind.clone())
                .unwrap_or_else(|| panic!("文本 {t} 应在 IR 中"))
        };
        assert_eq!(kind_of("旁注一行"), RegionKind::Aside);
        assert_eq!(kind_of("x = f(a)"), RegionKind::Code);
        assert_eq!(kind_of("〔1〕文献一"), RegionKind::Reference);
        assert_eq!(kind_of("〔2〕文献二"), RegionKind::Reference);
        assert_eq!(kind_of("正文一行"), RegionKind::Body);
        assert_eq!(kind_of("脚注一行"), RegionKind::Footnote);
        // 回贴同时清标题级别（aside/code/reference 无级别语义，对齐
        // mark_layout_index）。
        let aside = out.iter().find(|r| r.text == "旁注一行").unwrap();
        assert_eq!(aside.heading_level, None);
        // 空框列表：零成本直返（多数页面常态）。
        let same = mark_layout_kinds(
            vec![Region::new(0.0, 1.0, 0.0, 1.0, "原样")],
            &[],
            scale,
        );
        assert_eq!(same[0].kind, RegionKind::Body);
    }

    /// #10 chart 票：chart bbox 内的行回贴 [`RegionKind::Chart`]，
    /// **bbox 外的图注行保持 Body**（图注是独立 `FigureTitle` 元素）。
    ///
    /// 几何按 synth_samples.pdf 第 1 页实测等比缩放：chart 元素
    /// `[129,358]-[696,642]`、图注 `figure_title` 在其上方（y=340-358，
    /// 中心 349 <chart 框内）与下方（y=665-682）。
    #[test]
    fn chart_bbox_rows_marked_and_captions_stay_body() {
        let scale = (100.0_f32, 100.0_f32, 100.0_f32, 100.0_f32);
        let bb_chart = BoundingBox::from_coords(12.9, 35.8, 69.6, 64.2);
        let bboxes = vec![(&bb_chart, RegionKind::Chart)];
        let regions = vec![
            // 图注（图上方，中心 y=34.9 在框外）→ 保持 Body
            Region::new(33.9, 53.3, 34.0, 35.8, "图3-1分季度营业收入与成本对比"),
            // 图内文字三行（中心都在框内）→ Chart
            Region::new(20.0, 30.0, 36.7, 38.3, "171.2"),
            Region::new(20.0, 30.0, 38.0, 39.8, "160"),
            Region::new(20.0, 30.0, 40.5, 42.5, "140"),
            // 图注（图下方，中心 y=67.4 在框外）→ 保持 Body
            Region::new(25.0, 57.5, 66.5, 68.2, "图3-1分季度营业收入与成本对比 数据来源：内部财务台账"),
        ];
        let out = mark_layout_kinds(regions, &bboxes, scale);
        let kind_of = |t: &str| {
            out.iter()
                .find(|r| r.text == t)
                .map(|r| r.kind.clone())
                .unwrap_or_else(|| panic!("文本 {t} 应在 IR 中"))
        };
        assert_eq!(kind_of("171.2"), RegionKind::Chart);
        assert_eq!(kind_of("160"), RegionKind::Chart);
        assert_eq!(kind_of("140"), RegionKind::Chart);
        // 图注未被chart 框吞掉——这正是"图注靠独立 FigureTitle 元素、
        // 不靠从 chart 块 text 里剥离"这条决策的几何前提。
        assert_eq!(kind_of("图3-1分季度营业收入与成本对比"), RegionKind::Body);
        assert_eq!(
            kind_of("图3-1分季度营业收入与成本对比 数据来源：内部财务台账"),
            RegionKind::Body
        );
    }

    // ── #9 修法 4：表内单元格"同一内容两份"去重 ──

    /// 表内公式：纯文本 + LaTeX 两份 → 取 LaTeX 那份（第 0 步对拍的 `X<br/>$X$`）。
    #[test]
    fn cell_formula_double_entry_deduped() {
        let html = "<table><tr><td>a</td><td>\\overline{{f(x)}}=x^{2}+1<br/>$\\overline{{f(x)}}=x^{2}+1$</td></tr></table>";
        let out = simplify_table_html(html);
        assert!(
            out.contains("<td>$\\overline{{f(x)}}=x^{2}+1$</td>"),
            "应只留 LaTeX 一份，got: {out}"
        );
        assert!(!out.contains("<br/>"));
    }

    /// 保守性：正常的 `<br/>` 多行单元格（两段内容不同）**不**被合并。
    #[test]
    fn cell_multiline_untouched_when_parts_differ() {
        let html = "<table><tr><td>甲<br/>乙</td><td>x<br/>y</td></tr></table>";
        assert_eq!(simplify_table_html(html), html);
    }

    /// 保守性：三段（`<br/>` 两次）不处理；单段无 `<br/>` 不处理。
    #[test]
    fn cell_dedup_only_for_exactly_two_parts() {
        assert_eq!(simplify_table_html("<table><tr><td>a<br/>$a$<br/>b</td></tr></table>"), "<table><tr><td>a<br/>$a$<br/>b</td></tr></table>");
        assert_eq!(simplify_table_html("<table><tr><td>单价</td></tr></table>"), "<table><tr><td>单价</td></tr></table>");
    }

    /// 非 table 输入（兜底分支）同样过去重，且不破坏原文。
    #[test]
    fn simplify_keeps_plain_text_input() {
        assert_eq!(simplify_table_html("裸文本"), "裸文本");
    }

    // ── C 线：表内嵌图（table-with-image）→ 本仓网格重建 ──
    //
    // 修法：`reconstruct_table_with_embedded_image` 对「table bbox 内含 Image
    // 元素」的表改走 `table_grid` 网格重建，绕开被内嵌图污染的上游
    // `html_structure`；不含图 / 重建失败一律回退原路径（golden 零漂移的依据）。

    /// 上游被内嵌图污染的 `html_structure`：`72.4%` 的 OCR 框被 cell 边界切开，
    /// 按宽度比例分配后变成 `7` + `2.4%`（synth_samples.pdf 第 2 页取证）。
    const BROKEN_HTML: &str = "<table><tr><td>产品线</td><td>2024Q2</td><td>7</td>\
         <td>2.4%</td></tr><tr><td>智能终端</td><td>72.4%</td></tr></table>";

    /// 4 列 × 5 行表 + 落在 table bbox 内、**内部零文字**的 Image 元素。
    ///
    /// 几何按 synth_samples.pdf 第 2 页等比复刻，两个细节是回归的关键：
    /// 1. 行距刻意双峰 `[38,77,75,36]`（含图那行被图撑高）——`relative_row_tol`
    ///    的中位数会落在 77 上把末两行并成一行，故新路径必须走 p25 稳健估计；
    /// 2. 额外给一条表外图注文本，把 text 侧 `th` 拉到与 layout 侧 `lh` 同量级
    ///    ——否则 `norm_membership` 归一化后表 bbox 盖不住首行（真实页不是这样）。
    ///
    /// `col0_xs`  lets 单测注入首列 x 抖动，构造「列不齐 → 重建失败」样本。
    fn page_table_with_embedded_image(col0_xs: [f32; 5], with_image: bool) -> StructureResult {
        let rows: [[&str; 4]; 5] = [
            ["产品线", "2024Q2", "2024Q3", "趋势"],
            ["智能终端", "72.4%", "78.1%", ""],
            // 第 4 列此行是图（真值里是 <img class="spark">），故无文本。
            ["工业模组", "64.9%", "69.3%", ""],
            ["车载电子", "81.2%", "83.5%", "—"],
            ["新能源组件", "58.7%", "66.4%", "↑"],
        ];
        let row_y = [120.0_f32, 158.0, 235.0, 310.0, 346.0];
        let mut trs = Vec::new();
        let mut cells = Vec::new();
        for (ri, row) in rows.iter().enumerate() {
            for (ci, text) in row.iter().enumerate() {
                cells.push(cell(ri, ci, text));
                if text.is_empty() {
                    continue;
                }
                let x = if ci == 0 { col0_xs[ri] } else { 260.0 + (ci as f32 - 1.0) * 140.0 };
                // 首列窄（60）、其余列宽（120）：留出列间空隙，使 `col0_xs` 的
                // 抖动在测试里只影响「列 x 对齐」判据，不会先触发行内聚类合并。
                let w = if ci == 0 { 60.0 } else { 120.0 };
                trs.push(tr(x, row_y[ri], x + w, row_y[ri] + 20.0, text));
            }
        }
        // 表外图注：只影响 `page_scale` 的分母尺度，不进网格重建（非表内文本）。
        trs.push(tr(20.0, 280.0, 80.0, 300.0, "（图注）"));
        StructureResult {
            layout_elements: if with_image {
                // 图占第 4 列第 2 行的格位——该格在真值里是 <img class="spark">，
                // 无文本。y 区间取得与第 3/4 行（第 4 列有 `—`/`↑`）不重叠。
                vec![image_el(540.0, 180.0, 660.0, 240.0)]
            } else {
                Vec::new()
            },
            text_regions: Some(trs),
            tables: vec![TableResult::new(
                BoundingBox::from_coords(100.0, 100.0, 700.0, 390.0),
                TableType::Wireless,
            )
            .with_cells(cells)
            .with_html_structure(BROKEN_HTML)],
            ..StructureResult::new("t", 0)
        }
    }

    fn table_htmls(page: &StructureResult) -> Vec<String> {
        to_docir(std::slice::from_ref(page), &[])
            .pages
            .iter()
            .flat_map(|p| p.regions.iter())
            .filter(|r| matches!(r.kind, RegionKind::TableHtml))
            .map(|r| r.text.clone())
            .collect()
    }

    /// 表内含图 → 走网格重建：`72.4%` 完整（不被切成 `7` + `2.4%`），
    /// 且输出与被污染的上游 `html_structure` 无关。
    #[test]
    fn embedded_image_table_uses_grid_reconstruction() {
        let page = page_table_with_embedded_image([120.0; 5], true);
        let htmls = table_htmls(&page);
        assert_eq!(htmls.len(), 1, "表不得丢失");
        let html = &htmls[0];
        assert!(html.contains("<td>72.4%</td>"), "72.4% 应完整: {html}");
        assert!(!html.contains(">7</td>"), "不得出现被切开的 7: {html}");
        assert_ne!(html, BROKEN_HTML, "不应复用上游 html_structure");
        // 其余被切碎的单元格同样完整。
        for expect in ["智能终端", "78.1%", "64.9%", "69.3%", "81.2%", "83.5%", "58.7%", "66.4%"] {
            assert!(html.contains(expect), "{expect} 缺失: {html}");
        }
        // 末两行未被并成一行（`车载电子新能源组件` 是行距双峰误并的症状）。
        assert!(!html.contains("车载电子新能源组件"), "行被误并: {html}");
    }

    /// 不含图 → 逐字节走原 `html_structure` 路径（golden 零漂移的机制保证）。
    #[test]
    fn table_without_image_keeps_html_structure() {
        let page = page_table_with_embedded_image([120.0; 5], false);
        assert_eq!(table_htmls(&page), vec![simplify_table_html(BROKEN_HTML)]);
    }

    /// 含图但列 x 参差（首列散布 40 > `0.02*page_w`）→ 重建返回 `None` →
    /// 回退 `html_structure`，表不丢。
    #[test]
    fn embedded_image_table_falls_back_when_columns_misaligned() {
        let page = page_table_with_embedded_image([120.0, 130.0, 140.0, 150.0, 160.0], true);
        assert_eq!(table_htmls(&page), vec![simplify_table_html(BROKEN_HTML)]);
    }
}
