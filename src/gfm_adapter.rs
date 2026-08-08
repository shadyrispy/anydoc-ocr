//! StructureResult → GFM（图片型 PDF/OFD 的 OCR 结果）
//!
//! 主路径直接读取 OCR 的 `text_regions`（按阅读顺序拼接），而非依赖
//! `StructureResult::to_markdown()`。原因：版面模型（PP-DocLayout）常把
//! 整页图片型文档误判为 `Header`/`Footer`，而 `to_markdown()` 会跳过这些
//! 类型，导致正文丢失。文本区域是 OCR 的直接、可靠结果，不受版面语义分类影响。
//! 表格区域单独用 `html_structure` 输出，并剔除落在表格内的文本区域以防重复。
use oar_ocr::domain::structure::StructureResult;

/// 多页 StructureResult 转为 GFM 文本。
pub fn structure_results_to_gfm(pages: &[StructureResult]) -> String {
    let debug = std::env::var("ANYDOC_DEBUG_GFM").is_ok();
    let mut out = String::new();
    for (pi, page) in pages.iter().enumerate() {
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
                let in_table = page.tables.iter().any(|tb| {
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
        for t in order_text_regions(&regions) {
            out.push_str(&t);
            out.push('\n');
        }
        for table in &page.tables {
            if let Some(html) = &table.html_structure {
                out.push_str("\n\n");
                out.push_str(&simplify_table_html(html));
            }
        }
    }
    out.trim_end().to_string()
}

/// OCR 文本区域的阅读顺序还原。
///
/// 旧逻辑按 `(y, x)` 单排序，双列页面会左/右列逐行交错（左1→右1→左2→右2），
/// 正文阅读顺序被打乱。这里改为：把所有文本区域按 x 中心排序，取**最大间隙**
/// 切分出列——双列页面的列间 gutter 正是最大间隙；单栏页面的最大间隙很小（只是
/// 列内相邻行的 x 抖动），不会触发切分。每列内按 y 排序、列间从左到右。
///
/// 关键点：列检测用**所有**文本区域（含宽条目）。早先版本把跨度>60% 页宽的元素
/// 当作"整宽"剔除，但公报目录里左列的长标题（如"上海市人民政府办公厅关于转发
/// 市交通委制订的《上海港口基础设施维护管理办法》的通知"）跨度就达~68% 页宽，
/// 被误删后列间隙被糊掉、分栏失败。因此只把**真正跨整页**（x 同时贴近左右边距）
/// 的元素（页眉/页脚/通栏标题）剔除为 header/footer，左列长条目按其 x 中心自然
/// 归入左列。
///
/// 无清晰间隙（单栏或无法切分）时退化为纯 y 排序，兼容单栏文档与边缘情况。
///
/// `regions`: `(x_min, x_max, y_min, y_max, 文本)`
fn order_text_regions(regions: &[(f32, f32, f32, f32, String)]) -> Vec<String> {
    if regions.is_empty() {
        return Vec::new();
    }
    let page_w = regions.iter().map(|r| r.1).fold(0.0_f32, f32::max);
    if page_w <= 0.0 {
        return sort_by_y(regions);
    }

    // 真正跨整页的元素（页眉/页脚/通栏标题）：x 同时贴近左右边距，才从列检测中剔除。
    // 仅按"跨度>60% 页宽"判定会误删左列长条目（目录长标题），导致分栏失败。
    let is_full = |r: &(f32, f32, f32, f32, String)| {
        (r.1 - r.0) > 0.92 * page_w && r.0 < 0.08 * page_w
    };

    // 正文区域：(中心x, y, 文本)；跨整页元素：(y, 文本)
    let mut body: Vec<(f32, f32, String)> = regions
        .iter()
        .filter(|r| !is_full(r))
        .map(|r| ((r.0 + r.1) / 2.0, r.2, r.4.clone()))
        .collect();
    let full: Vec<(f32, String)> = regions
        .iter()
        .filter(|r| is_full(r))
        .map(|r| (r.2, r.4.clone()))
        .collect();

    // 列检测：正文按中心 x 排序，最大间隙即列间 gutter。要求 gutter >= 3% 页宽且
    // 两侧各 >=2 区域，避免把单栏内的大间距误判为分栏。
    if body.len() >= 4 {
        body.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut best_gap = 0.0_f32;
        let mut best_i = 1usize;
        for i in 1..body.len() {
            let g = body[i].0 - body[i - 1].0;
            if g > best_gap {
                best_gap = g;
                best_i = i;
            }
        }
        let min_gap = 0.03 * page_w;
        if best_gap >= min_gap && best_i >= 2 && (body.len() - best_i) >= 2 {
            let split = (body[best_i - 1].0 + body[best_i].0) / 2.0;
            let mut left: Vec<(f32, String)> =
                body.iter().filter(|b| b.0 < split).map(|b| (b.1, b.2.clone())).collect();
            let mut right: Vec<(f32, String)> =
                body.iter().filter(|b| b.0 >= split).map(|b| (b.1, b.2.clone())).collect();
            left.sort_by(ord_y);
            right.sort_by(ord_y);

            // 整宽元素按 y 归页眉(y<正文起点)/页脚(y>正文终点)/正文区间(罕见置后)
            let body_min = left
                .iter()
                .chain(right.iter())
                .map(|(y, _)| *y)
                .fold(f32::INFINITY, f32::min);
            let body_max = left
                .iter()
                .chain(right.iter())
                .map(|(y, _)| *y)
                .fold(f32::NEG_INFINITY, f32::max);
            let mut head: Vec<_> = full.iter().filter(|(y, _)| *y < body_min).cloned().collect();
            let mut foot: Vec<_> = full.iter().filter(|(y, _)| *y > body_max).cloned().collect();
            let mut mid: Vec<_> = full
                .iter()
                .filter(|(y, _)| *y >= body_min && *y <= body_max)
                .cloned()
                .collect();
            head.sort_by(ord_y);
            mid.sort_by(ord_y);
            foot.sort_by(ord_y);

            let mut out: Vec<String> = Vec::new();
            for (_, t) in head {
                out.push(t);
            }
            for (_, t) in left {
                out.push(t);
            }
            for (_, t) in right {
                out.push(t);
            }
            for (_, t) in mid {
                out.push(t);
            }
            for (_, t) in foot {
                out.push(t);
            }
            return out;
        }
    }
    sort_by_y(regions)
}

fn ord_y(a: &(f32, String), b: &(f32, String)) -> std::cmp::Ordering {
    a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal)
}

/// 单栏/无可切分列时的退化为纯 y 排序（保持旧行为兼容）
fn sort_by_y(regions: &[(f32, f32, f32, f32, String)]) -> Vec<String> {
    let mut v: Vec<(f32, String)> = regions.iter().map(|r| (r.2, r.4.clone())).collect();
    v.sort_by(ord_y);
    v.into_iter().map(|(_, t)| t).collect()
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
    use super::order_text_regions;

    /// 构造区域：(x_min, x_max, y_min, y_max=+10, 文本)
    fn reg(x0: f32, x1: f32, y0: f32, t: &str) -> (f32, f32, f32, f32, String) {
        (x0, x1, y0, y0 + 10.0, t.to_string())
    }

    #[test]
    fn two_column_left_then_right() {
        // 页宽 1000：左列 50..450，右列 550..950，中间 12% 间隙
        let regions = vec![
            reg(50.0, 450.0, 100.0, "L1"),
            reg(50.0, 450.0, 200.0, "L2"),
            reg(50.0, 450.0, 300.0, "L3"),
            reg(550.0, 950.0, 100.0, "R1"),
            reg(550.0, 950.0, 200.0, "R2"),
            reg(550.0, 950.0, 300.0, "R3"),
        ];
        assert_eq!(
            order_text_regions(&regions),
            vec!["L1", "L2", "L3", "R1", "R2", "R3"]
        );
    }

    #[test]
    fn fullwidth_header_and_footer_excluded_from_columns() {
        // 整宽页眉(上)/页脚(下)会糊掉列间隙，必须剔除后才能正确分栏
        let regions = vec![
            reg(0.0, 1000.0, 20.0, "HEADER"),
            reg(50.0, 450.0, 100.0, "L1"),
            reg(50.0, 450.0, 200.0, "L2"),
            reg(550.0, 950.0, 100.0, "R1"),
            reg(550.0, 950.0, 200.0, "R2"),
            reg(0.0, 1000.0, 400.0, "FOOTER"),
        ];
        assert_eq!(
            order_text_regions(&regions),
            vec!["HEADER", "L1", "L2", "R1", "R2", "FOOTER"]
        );
    }

    #[test]
    fn single_column_fallback_by_y() {
        let regions = vec![
            reg(50.0, 450.0, 300.0, "A"),
            reg(50.0, 450.0, 100.0, "B"),
            reg(50.0, 450.0, 200.0, "C"),
        ];
        assert_eq!(order_text_regions(&regions), vec!["B", "C", "A"]);
    }

    #[test]
    fn no_interleave_when_columns_present() {
        // 复现真实 bug：左列 1..5 与右列 1..5 逐行交错，须还原为左全→右全
        let regions = vec![
            reg(50.0, 450.0, 100.0, "L1"),
            reg(550.0, 950.0, 105.0, "R1"), // y 相近，旧逻辑会插到 L1 后
            reg(50.0, 450.0, 200.0, "L2"),
            reg(550.0, 950.0, 205.0, "R2"),
            reg(50.0, 450.0, 300.0, "L3"),
            reg(550.0, 950.0, 305.0, "R3"),
        ];
        assert_eq!(
            order_text_regions(&regions),
            vec!["L1", "L2", "L3", "R1", "R2", "R3"]
        );
    }

    #[test]
    fn tight_gutter_two_column() {
        // 双列但 gutter 仅 4%（小于旧 4% 合并阈值），仍应正确分栏
        let regions = vec![
            reg(50.0, 480.0, 100.0, "L1"),
            reg(50.0, 480.0, 200.0, "L2"),
            reg(520.0, 950.0, 100.0, "R1"),
            reg(520.0, 950.0, 200.0, "R2"),
        ];
        assert_eq!(order_text_regions(&regions), vec!["L1", "L2", "R1", "R2"]);
    }
}
