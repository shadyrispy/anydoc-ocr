//! StructureResult → GFM（图片型 PDF/OFD 的 OCR 结果）
//!
//! 主路径直接读取 OCR 的 `text_regions`（按阅读顺序拼接），而非依赖
//! `StructureResult::to_markdown()`。原因：版面模型（PP-DocLayout）常把
//! 整页图片型文档误判为 `Header`/`Footer`，而 `to_markdown()` 会跳过这些
//! 类型，导致正文丢失。文本区域是 OCR 的直接、可靠结果，不受版面语义分类影响。
//! 表格区域单独用 `html_structure` 输出，并剔除落在表格内的文本区域以防重复。
//!
//! 阅读顺序由公共模块 `crate::reading_order` 还原（双列感知），与文字层通路共用。
use crate::reading_order::{order_text_regions, postprocess_lines, title_level};
use oar_ocr::domain::structure::{StructureResult, TableResult};

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
    if n_cols == 2 {
        let non_empty: Vec<&str> = table
            .cells
            .iter()
            .filter_map(|c| c.text.as_ref().map(|t| t.trim()).filter(|t| !t.is_empty()))
            .collect();
        if !non_empty.is_empty() {
            let long = non_empty
                .iter()
                .filter(|t| t.chars().count() >= 15 || t.ends_with(['。', '，', '；', '：']))
                .count();
            if long as f32 / non_empty.len() as f32 > 0.6 {
                return true;
            }
        }
    }
    false
}

/// 多页 StructureResult 转为 GFM 文本。
pub fn structure_results_to_gfm(pages: &[StructureResult]) -> String {
    let debug = std::env::var("ANYDOC_DEBUG_GFM").is_ok();
    let mut out = String::new();
    for (pi, page) in pages.iter().enumerate() {
        // 仅接受通过伪表格过滤的表格：被拒绝的误判表格既不入 HTML，也不
        // 从文本区域中剔除，其区域照常拼入正文行，避免正文丢失。
        let tables: Vec<&TableResult> = page
            .tables
            .iter()
            .filter(|t| !is_false_positive_table(t))
            .collect();
        // 收集文本区域（剔除落在表格内的，避免与表格 HTML 重复）
        let mut regions: Vec<(f32, f32, f32, f32, String)> = Vec::new();
        if let Some(regs) = &page.text_regions {
            for r in regs {
                let Some(t) = r.text.as_ref() else { continue };
                let t = t.trim();
                if t.is_empty() {
                    continue;
                }
                let b = &r.bounding_box;
                let in_table = tables.iter().any(|tb| {
                    let tb = &tb.bbox;
                    b.x_min() >= tb.x_min()
                        && b.x_max() <= tb.x_max()
                        && b.y_min() >= tb.y_min()
                        && b.y_max() <= tb.y_max()
                });
                if in_table {
                    continue;
                }
                regions.push((b.x_min(), b.x_max(), b.y_min(), b.y_max(), t.to_string()));
            }
        }
        if debug && pi < 7 {
            let pw = regions.iter().map(|r| r.1).fold(0.0_f32, f32::max);
            eprintln!("[gfm-dbg] page={pi} page_w={pw:.0} n_regions={}", regions.len());
            for (x0, x1, y0, y1, t) in &regions {
                let cx = (x0 + x1) / 2.0;
                let wide = (x1 - x0) > 0.6 * pw;
                eprintln!(
                    "[gfm-dbg]   x0={x0:6.0} x1={x1:6.0} cx={cx:6.0} y0={y0:6.0} y1={y1:6.0} wide={wide} | {t}"
                );
            }
        }
        let lines = postprocess_lines(order_text_regions(&regions));
        for t in apply_title_prefixes(lines, page) {
            out.push_str(&t);
            out.push('\n');
        }
        for table in &tables {
            if let Some(html) = &table.html_structure {
                out.push_str("\n\n");
                out.push_str(&simplify_table_html(html));
            }
        }
    }
    out.trim_end().to_string()
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
    if titles.is_empty() {
        return lines;
    }
    lines
        .into_iter()
        .map(|line| {
            if line.trim_start().starts_with('#') {
                return line;
            }
            for (tt, lv) in &titles {
                let lt = line.trim();
                if lt == tt || lt.contains(tt.as_str()) || tt.contains(lt) {
                    return format!("{} {}", "#".repeat(*lv), line);
                }
            }
            line
        })
        .collect()
}

/// 剥离 oar-ocr 表格 HTML 的 `<html>/<body>` 包裹（若有），仅保留 `<table>`。
fn simplify_table_html(html: &str) -> String {
    let h = html.trim();
    if let (Some(s), Some(e)) = (h.find("<table"), h.rfind("</table>")) {
        // 防御畸形结构（</table> 先于 <table> 出现），避免切片越界 panic
        if s <= e {
            let end = (e + 7).min(h.len());
            return h[s..end].to_string();
        }
    }
    h.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use oar_ocr::domain::structure::{TableCell, TableType};
    use oar_ocr::processors::BoundingBox;

    fn cell(row: usize, col: usize, text: &str) -> TableCell {
        TableCell::new(BoundingBox::from_coords(0.0, 0.0, 10.0, 10.0), 1.0)
            .with_position(row, col)
            .with_text(text)
    }

    fn table(cells: Vec<TableCell>) -> TableResult {
        TableResult::new(BoundingBox::from_coords(0.0, 0.0, 100.0, 100.0), TableType::Wireless)
            .with_cells(cells)
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
            cell(0, 0, "a"), cell(0, 1, "b"), cell(0, 2, "c"),
            cell(1, 0, "1"), cell(1, 1, "2"), cell(1, 2, "3"),
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
