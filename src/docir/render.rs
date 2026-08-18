//! DocIR → GFM 渲染（P1.5/AC-6）：只消费 [`DocIR`]，不依赖 pdf/ofd 内部类型或
//! StructureResult。三源历史装配差异收敛为按 [`PageSource`] 分流的渲染风格，
//! 字节级行为与旧 `emitter` 通路一致（golden 守护，AC-8）：
//!
//! - 正文行（`Body`）：TextLayerPdf/Ocr 保留标题空行语义（`#` 行前后空行），
//!   TextLayerOfd 为朴素 `join("\n")`（历史无空行语义）；
//! - 网格表（`Grid`，跨页合并 pass 已定格在首表页）：文字层源 `html + "\n\n"`
//!   （表格独占页），Ocr 源 `"\n\n" + html + "\n"`（表格与正文共存于首表页）；
//! - 表格 HTML（`TableHtml`，Ocr 源）：正文后空行 + html，多表间空行分隔；
//! - 成品块（`PreRendered`）：producer 已含精确分隔符，原样追加，不二次加工。

use std::collections::BTreeMap;

use crate::docir::{DocIR, PageSource};
use crate::region::{Region, RegionKind};
use crate::table_grid::table_grid_to_html;

/// 渲染 DocIR 为 GFM 文本：按页分段（页号升序），段间空行，段两端 trim
/// （与旧 `DocumentEmitter::finish` 一致，对齐 GFM 块语义）。
pub(crate) fn render(doc: &DocIR) -> String {
    let mut segments: BTreeMap<u32, String> = BTreeMap::new();
    for page in &doc.pages {
        let mut seg = String::new();
        // 1) 正文行
        let bodies: Vec<&str> = page
            .regions
            .iter()
            .filter(|r| r.kind == RegionKind::Body)
            .map(|r| r.text.as_str())
            .collect();
        match page.source {
            // OFD 文字层：朴素单换行拼接（历史行为，无标题空行语义）。
            PageSource::TextLayerOfd => {
                if !bodies.is_empty() {
                    seg.push_str(&bodies.join("\n"));
                }
            }
            // PDF 文字层 / OCR：标题（# 开头）前后空行，正文行段落内单换行。
            PageSource::TextLayerPdf | PageSource::Ocr => {
                for t in bodies {
                    let is_heading = t.starts_with('#');
                    if is_heading && !seg.is_empty() && !seg.ends_with("\n\n") {
                        seg.push('\n');
                    }
                    seg.push_str(t);
                    seg.push('\n');
                    if is_heading {
                        seg.push('\n');
                    }
                }
            }
        }
        // 2) 成品块：原样追加（producer 已嵌入精确分隔符）
        for r in regions_of(page, |k| matches!(k, RegionKind::PreRendered)) {
            seg.push_str(&r.text);
        }
        // 3) OCR 识别表 HTML：正文后空行 + html（多表间以空行分隔）
        if page.source == PageSource::Ocr {
            for r in regions_of(page, |k| matches!(k, RegionKind::TableHtml)) {
                if !seg.ends_with("\n\n") {
                    seg.push_str("\n\n");
                }
                seg.push_str(&r.text);
            }
        }
        // 4) 网格表（跨页合并后定格在本页）
        for r in regions_of(page, |k| matches!(k, RegionKind::Grid(_))) {
            let html = grid_html(&r);
            match page.source {
                // 文字层：表格独占一页，html + "\n\n"。
                PageSource::TextLayerPdf | PageSource::TextLayerOfd => {
                    seg.push_str(&html);
                    seg.push_str("\n\n");
                }
                // Ocr：表格与正文共存于首表页，"\n\n" + html + "\n"。
                PageSource::Ocr => {
                    seg.push_str("\n\n");
                    seg.push_str(&html);
                    seg.push('\n');
                }
            }
        }
        segments.entry(page.page_no).or_default().push_str(&seg);
    }
    // 收尾：页号升序拼接，段间空行，段两端 trim（旧 finish 语义）。
    let mut out = String::new();
    for (_, seg) in segments {
        let s = seg.trim();
        if s.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(s);
    }
    out
}

/// 按 kind 谓词依序取区块引用。
fn regions_of(
    page: &crate::docir::PageIR,
    pred: impl Fn(&RegionKind) -> bool,
) -> impl Iterator<Item = &Region> {
    page.regions.iter().filter(move |r| pred(&r.kind))
}

/// 取 Grid 区块的 HTML（调用处已由谓词保证类型）。
fn grid_html(r: &Region) -> String {
    match &r.kind {
        RegionKind::Grid(g) => table_grid_to_html(g),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docir::PageIR;
    use crate::table_grid::{TableCell, TableGrid};

    fn cell(t: &str) -> TableCell {
        TableCell {
            text: t.into(),
            x: 0.0,
            y: 0.0,
            h: 10.0,
        }
    }

    fn grid(cols: usize, texts: &[&str]) -> TableGrid {
        TableGrid {
            cols,
            header: vec![],
            rows: texts.chunks(cols).map(|c| c.iter().map(|s| cell(s)).collect()).collect(),
            has_header: false,
        }
    }

    fn page(source: PageSource, regions: Vec<Region>) -> PageIR {
        PageIR {
            page_no: 0,
            regions,
            source,
        }
    }

    /// PDF/Ocr 正文行：标题（# 开头）前补空行、后加空行；正文行单换行。
    #[test]
    fn heading_blank_line_semantics_for_pdf_and_ocr() {
        let regions = vec![
            Region::new(0.0, 1.0, 0.0, 1.0, "## 标题"),
            Region::new(0.0, 1.0, 1.0, 2.0, "正文行"),
            Region::new(0.0, 1.0, 2.0, 3.0, "# 又一标题"),
        ];
        let doc = DocIR {
            pages: vec![page(PageSource::TextLayerPdf, regions)],
        };
        let out = render(&doc);
        // trim 后：标题行\n\n正文行\n\n# 又一标题（标题前空行体现在块间 \n\n）
        assert!(out.starts_with("## 标题\n\n正文行\n\n# 又一标题"), "got: {out}");
    }

    /// OFD 正文行：朴素 join("\n")，标题不加空行。
    #[test]
    fn ofd_body_joins_with_single_newline() {
        let regions = vec![
            Region::new(0.0, 1.0, 0.0, 1.0, "## 标题"),
            Region::new(0.0, 1.0, 1.0, 2.0, "正文行"),
        ];
        let doc = DocIR {
            pages: vec![page(PageSource::TextLayerOfd, regions)],
        };
        assert_eq!(render(&doc), "## 标题\n正文行");
    }

    /// 文字层网格表 flush 格式：html + "\n\n"（表格独占页）。
    #[test]
    fn text_layer_grid_flush_format() {
        let regions = vec![Region::new(0.0, 0.0, 0.0, 0.0, String::new())
            .with_kind(RegionKind::Grid(grid(2, &["a", "b"])))];
        let doc = DocIR {
            pages: vec![page(PageSource::TextLayerPdf, regions)],
        };
        let out = render(&doc);
        assert!(out.contains("<table>"), "表格 HTML 输出");
        assert!(out.trim_end().ends_with("</table>"), "段尾 trim 后以 </table> 结束");
    }

    /// Ocr 网格表 flush 格式：正文行尾 `\n` + flush 前缀 `"\n\n"` + html + `"\n"`
    /// （表格与正文共存于首表页；与旧 emitter Gfm flush 字节一致）。
    #[test]
    fn ocr_grid_flush_after_body() {
        let regions = vec![
            Region::new(0.0, 1.0, 0.0, 1.0, "正文"),
            Region::new(0.0, 0.0, 0.0, 0.0, String::new())
                .with_kind(RegionKind::Grid(grid(2, &["a", "b"]))),
        ];
        let doc = DocIR {
            pages: vec![page(PageSource::Ocr, regions)],
        };
        let out = render(&doc);
        let idx = out.find("正文").unwrap();
        assert!(
            out[idx..].starts_with("正文\n\n\n<table"),
            "Ocr 网格表前须空行（正文行尾 \\n + flush \\n\\n），got: {}",
            &out[idx..]
        );
    }

    /// Ocr 表格 HTML：正文行尾 `\n` + `"\n\n"` 补齐 + html；多表间 2 个换行分隔
    /// （与旧 gfm 通路的 `ends_with("\n\n")` 补齐逻辑字节一致）。
    #[test]
    fn ocr_table_html_blank_line_separated() {
        let regions = vec![
            Region::new(0.0, 1.0, 0.0, 1.0, "正文"),
            Region::new(0.0, 0.0, 0.0, 0.0, "<table><tr><td>t1</td></tr></table>")
                .with_kind(RegionKind::TableHtml),
            Region::new(0.0, 0.0, 0.0, 0.0, "<table><tr><td>t2</td></tr></table>")
                .with_kind(RegionKind::TableHtml),
        ];
        let doc = DocIR {
            pages: vec![page(PageSource::Ocr, regions)],
        };
        let out = render(&doc);
        assert!(out.contains("正文\n\n\n<table><tr><td>t1</td></tr></table>"));
        assert!(out.contains("</table>\n\n<table><tr><td>t2</td></tr></table>"));
    }

    /// PreRendered：原样追加（含 producer 嵌入的前导/尾随分隔符）。
    #[test]
    fn pre_rendered_appended_verbatim() {
        let regions = vec![
            Region::new(0.0, 1.0, 0.0, 1.0, "正文行"),
            Region::new(0.0, 0.0, 0.0, 0.0, "\n| 管道表 |\n")
                .with_kind(RegionKind::PreRendered),
        ];
        let doc = DocIR {
            pages: vec![page(PageSource::TextLayerPdf, regions)],
        };
        let out = render(&doc);
        assert!(out.contains("正文行\n\n| 管道表 |"), "got: {out}");
    }

    /// 多页按页号升序拼接、段间空行（旧 finish 语义）。
    #[test]
    fn pages_concatenated_in_order_with_blank_lines() {
        let doc = DocIR {
            pages: vec![
                PageIR {
                    page_no: 2,
                    regions: vec![Region::new(0.0, 1.0, 0.0, 1.0, "third")],
                    source: PageSource::TextLayerOfd,
                },
                PageIR {
                    page_no: 0,
                    regions: vec![Region::new(0.0, 1.0, 0.0, 1.0, "first")],
                    source: PageSource::TextLayerOfd,
                },
            ],
        };
        assert_eq!(render(&doc), "first\n\nthird");
    }

    /// 空页（无区块）跳过，不产生空段。
    #[test]
    fn empty_pages_skipped() {
        let doc = DocIR {
            pages: vec![
                PageIR {
                    page_no: 0,
                    regions: vec![],
                    source: PageSource::TextLayerOfd,
                },
                PageIR {
                    page_no: 1,
                    regions: vec![Region::new(0.0, 1.0, 0.0, 1.0, "内容")],
                    source: PageSource::TextLayerOfd,
                },
            ],
        };
        assert_eq!(render(&doc), "内容");
    }
}
