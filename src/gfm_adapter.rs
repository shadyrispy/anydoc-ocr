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
    norm_membership, order_structure, page_scale, postprocess_lines, title_level,
};
use crate::region::{Region, RegionKind};
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

/// 多页 StructureResult → DocIR（OCR 源 producer，P1.5）。
///
/// 每页产出 source=`Ocr` 的 [`PageIR`]：正文行（阅读顺序 + 标题前缀已应用）为
/// `Body` 区块、识别表 HTML 为 `TableHtml` 区块、Image 补救重建网格为 `Grid`
/// 区块。跨页 Grid 合并与 GFM 渲染由调用方经 `DocIR::render()` 统一承担
/// （与文字层表格的段式装配一致，保证阅读顺序）。
pub fn to_docir(pages: &[StructureResult]) -> DocIR {
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
                regions.push(
                    Region::new(b.x_min(), b.x_max(), b.y_min(), b.y_max(), t.to_string())
                        .with_confidence(r.confidence),
                );
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
        // 本页正文行（标题前缀已应用，# 前缀由 docir 渲染层识别空行语义）+
        // layout 表格 HTML。ADR-0009：块驱动阅读序 + 段落合并，postprocess 做
        // 连字符/全角归一，最后依据版面 title 块注入 markdown 标题前缀。
        let mut out: Vec<Region> =
            apply_title_prefixes(postprocess_lines(order_structure(page, &regions)), page)
                .into_iter()
                .map(|l| Region::new(0.0, 0.0, 0.0, 0.0, l))
                .collect();
        for table in &tables {
            if let Some(html) = &table.html_structure {
                out.push(
                    Region::new(0.0, 0.0, 0.0, 0.0, simplify_table_html(html))
                        .with_kind(RegionKind::TableHtml),
                );
            }
        }
        // Image 跨页表（Grid）：同列续接 / 换表定格 / 表格中断由 pass 承担
        if let Some(g) = img_grid {
            out.push(Region::new(0.0, 0.0, 0.0, 0.0, String::new()).with_kind(RegionKind::Grid(g)));
        }
        doc.push_page(pi as u32, PageSource::Ocr, out);
    }
    doc
}

/// 多页 StructureResult → GFM 文本（OCR 源便捷入口，P1.5）。
///
/// `to_docir` 产 IR → 跨页表合并 pass → 统一渲染。批量 OCR 主路径
/// （`convert_pdf_ocr` / 质量探针 / OFD 整页 OCR）经此获得与旧 emitter
/// 通路字节一致的输出（golden 守护，AC-8）。
pub fn to_markdown(pages: &[StructureResult]) -> String {
    let mut doc = to_docir(pages);
    crate::docir::passes::cross_page_table::run(&mut doc);
    doc.render()
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
        let out = to_markdown(&[p1, p2]);
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
