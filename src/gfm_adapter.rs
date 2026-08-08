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
    let mut out = String::new();
    for page in pages {
        let mut text: Vec<(f32, f32, String)> = Vec::new();
        if let Some(regions) = &page.text_regions {
            for r in regions {
                let Some(t) = r.text.as_ref() else { continue };
                let t = t.trim();
                if t.is_empty() {
                    continue;
                }
                let b = &r.bounding_box;
                // 落在表格 bbox 内的文本由表格 HTML 表达，跳过避免重复
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
                text.push((b.y_min(), b.x_min(), t.to_string()));
            }
        }
        text.sort_by(|a, b| {
            a.0.partial_cmp(&b.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        });
        for (_, _, t) in &text {
            out.push_str(t);
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
