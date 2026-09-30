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
//!
//! #6 第 2 步：`#` 前缀从 producer 下移到这里——标题级别是 IR 数据
//! （`Region.heading_level`），字面量由 [`Region::rendered_line`] 写出，空行语义
//! 由 [`Region::is_heading`] 决定。producer 不再往 `text` 里写 `#`，两视图逐条
//! 等价，故**本步输出逐字节不变**（BACKLOG #6 验收判据里的第 2 步）。

use std::collections::BTreeMap;

use crate::docir::{DocIR, PageSource};
use crate::region::{NoiseKind, Region, RegionKind};
use crate::table_grid::table_grid_to_html;

/// 渲染 DocIR 为 GFM 文本：按页分段（页号升序），段间空行，段两端 trim
/// （与旧 `DocumentEmitter::finish` 一致，对齐 GFM 块语义）。
pub(crate) fn render(doc: &DocIR) -> String {
    render_with_furniture(doc, false)
}

/// 家具/脚注（`Noise`/`Footnote`）的**可选输出**渲染（#10 例外项）。
///
/// `emit = false`（默认）：与 [`render`] 逐字节相同——`Noise`/`Footnote` 区块
/// 零消费（下面四个阶段都不匹配它们），"收集进 IR 但不输出"。
///
/// `emit = true`（CLI/env 开关 `ANYDOC_EMIT_FURNITURE`）：每页段末追加 HTML
/// 注释行——`<!-- header: … -->` / `<!-- footer: … -->` /
/// `<!-- page-number: … -->` / `<!-- seal: … -->` / `<!-- footnote: … -->`。
/// 用注释形态的原因：不污染可见 markdown 文本、可 grep、GFM 合法；正式的
/// 结构化出口是 #10/#11 的 content_list v2 投影（`PAGE_HEADER`/`PAGE_FOOTER`/
/// `PAGE_NUMBER` 独立 item），本分支是过渡形态。
///
/// 家具项按 `y_min` 升序输出（页眉在前、页脚在后，det 顺序不作保证）；
/// 文本中的 `-->` 会提前终止 HTML 注释，替换为 `->`（显示用标注，不做原文保真）。
pub(crate) fn render_with_furniture(doc: &DocIR, emit: bool) -> String {
    let mut segments: BTreeMap<u32, String> = BTreeMap::new();
    for page in &doc.pages {
        let mut seg = String::new();
        // 1) 正文行：`#` 前缀在此写出（#6 第 2 步下移到渲染层）。
        //    #10 INDEX：目次条目行（`Index`）与正文同道输出（保持阅读顺序），
        //    渲染形态改成 `- ` 列表项（MinerU v1 渲染同样写 `- ` + 可选锚点）。
        let bodies: Vec<&Region> = page
            .regions
            .iter()
            .filter(|r| matches!(r.kind, RegionKind::Body | RegionKind::Index))
            .collect();
        /// 单个正文/目次行的渲染形态：目次 `- `，其余 [`Region::rendered_line`]。
        fn body_line(r: &Region) -> String {
            if r.kind == RegionKind::Index {
                format!("- {}", r.text)
            } else {
                r.rendered_line().into_owned()
            }
        }
        match page.source {
            // OFD 文字层：朴素单换行拼接（历史行为，无标题空行语义）。
            PageSource::TextLayerOfd => {
                for (i, r) in bodies.iter().enumerate() {
                    if i > 0 {
                        seg.push('\n');
                    }
                    seg.push_str(&body_line(r));
                }
            }
            // PDF 文字层 / OCR：标题（渲染后 `#` 开头）前后空行，正文行段落内单换行。
            PageSource::TextLayerPdf | PageSource::Ocr => {
                let mut prev_index = false;
                for r in &bodies {
                    let is_index = r.kind == RegionKind::Index;
                    // GFM：列表块与前导块之间必须有空行，否则前一段会被吞进列表项。
                    if is_index != prev_index && !seg.is_empty() && !seg.ends_with("\n\n") {
                        seg.push('\n');
                    }
                    let is_heading = r.is_heading();
                    if is_heading && !seg.is_empty() && !seg.ends_with("\n\n") {
                        seg.push('\n');
                    }
                    seg.push_str(&body_line(r));
                    seg.push('\n');
                    if is_heading {
                        seg.push('\n');
                    }
                    prev_index = is_index;
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
        // #6 第 3 步：带 `continues_prev` 标记的块是"内容已并入前页表格"的占位，
        // **整块跳过**——跳过与旧通路的"物理删除"逐字节相同（单测
        // `absorbed_stub_renders_identically_to_deletion` 钉住），标记只为投影层留。
        for r in regions_of(page, |k| matches!(k, RegionKind::Grid(_)))
            .filter(|r| !r.is_continues_prev())
        {
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
        // 5) 家具/脚注（#10 例外项）：默认不输出（上面四阶段不匹配 Noise/Footnote，
        //    已天然跳过）；开关打开时段末追加注释行。占位变体 Image/Code/Formula/
        //    Index/Aside 同样零消费——producer 未产，新类别出现才加渲染分支。
        if emit {
            let mut furniture: Vec<&Region> = regions_of(page, |k| {
                matches!(k, RegionKind::Noise(_) | RegionKind::Footnote)
            })
            .collect();
            furniture.sort_by(|a, b| {
                a.y_min.partial_cmp(&b.y_min).unwrap_or(std::cmp::Ordering::Equal)
            });
            for r in furniture {
                let label = match &r.kind {
                    RegionKind::Noise(NoiseKind::Header) => "header",
                    RegionKind::Noise(NoiseKind::Footer) => "footer",
                    RegionKind::Noise(NoiseKind::PageNumber) => "page-number",
                    RegionKind::Noise(NoiseKind::Seal) => "seal",
                    RegionKind::Footnote => "footnote",
                    _ => continue,
                };
                let t = r.text.replace("-->", "->");
                if !seg.is_empty() && !seg.ends_with('\n') {
                    seg.push('\n');
                }
                seg.push_str(&format!("<!-- {label}: {t} -->\n"));
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
    use crate::docir::{PageDims, PageIR};
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
            dims: PageDims::default(),
        }
    }

    /// PDF/Ocr 正文行：标题（有 `heading_level`）前补空行、后加空行；正文行单换行。
    /// #6 第 2 步：标题是 IR 字段，`#` 字面量由本层写出。
    #[test]
    fn heading_blank_line_semantics_for_pdf_and_ocr() {
        let regions = vec![
            Region::new(0.0, 1.0, 0.0, 1.0, "标题").with_heading_level(Some(2)),
            Region::new(0.0, 1.0, 1.0, 2.0, "正文行"),
            Region::new(0.0, 1.0, 2.0, 3.0, "又一标题").with_heading_level(Some(1)),
        ];
        let doc = DocIR {
            pages: vec![page(PageSource::TextLayerPdf, regions)],
        };
        let out = render(&doc);
        // trim 后：标题行\n\n正文行\n\n# 又一标题（标题前空行体现在块间 \n\n）
        assert!(out.starts_with("## 标题\n\n正文行\n\n# 又一标题"), "got: {out}");
    }

    /// #6 第 2 步的等价性正身：**同一页**用 IR 级别表达标题，与旧通路把 `#`
    /// 字面量焊在 `text` 里，渲染结果必须逐字节相同（三种来源各钉一条）。
    #[test]
    fn level_and_literal_prefix_render_identically() {
        for source in [
            PageSource::TextLayerPdf,
            PageSource::TextLayerOfd,
            PageSource::Ocr,
        ] {
            let by_level = page(
                source,
                vec![
                    Region::new(0.0, 1.0, 0.0, 1.0, "一、总则").with_heading_level(Some(2)),
                    Region::new(0.0, 1.0, 1.0, 2.0, "正文行"),
                ],
            );
            let by_literal = page(
                source,
                vec![
                    Region::new(0.0, 1.0, 0.0, 1.0, "## 一、总则"),
                    Region::new(0.0, 1.0, 1.0, 2.0, "正文行"),
                ],
            );
            assert_eq!(
                render(&DocIR { pages: vec![by_level] }),
                render(&DocIR { pages: vec![by_literal] }),
                "source={source:?}：级别与字面量必须渲染同形"
            );
        }
    }

    /// OFD 正文行：朴素 join("\n")，标题不加空行（前缀照样由本层写）。
    #[test]
    fn ofd_body_joins_with_single_newline() {
        let regions = vec![
            Region::new(0.0, 1.0, 0.0, 1.0, "标题").with_heading_level(Some(2)),
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
                    dims: PageDims::default(),
                },
                PageIR {
                    page_no: 0,
                    regions: vec![Region::new(0.0, 1.0, 0.0, 1.0, "first")],
                    source: PageSource::TextLayerOfd,
                    dims: PageDims::default(),
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
                    dims: PageDims::default(),
                },
                PageIR {
                    page_no: 1,
                    regions: vec![Region::new(0.0, 1.0, 0.0, 1.0, "内容")],
                    source: PageSource::TextLayerOfd,
                    dims: PageDims::default(),
                },
            ],
        };
        assert_eq!(render(&doc), "内容");
    }

    // ── #10 例外项：家具/脚注的可选输出 ──

    fn noise_region(text: &str, y: f32, kind: RegionKind) -> Region {
        Region::new(0.0, 100.0, y, y + 5.0, text).with_kind(kind)
    }

    fn furniture_doc() -> DocIR {
        DocIR {
            pages: vec![PageIR {
                page_no: 0,
                regions: vec![
                    Region::new(0.0, 100.0, 40.0, 50.0, "正文"),
                    noise_region("页眉文本", 0.0, RegionKind::Noise(NoiseKind::Header)),
                    noise_region("页脚文本", 90.0, RegionKind::Noise(NoiseKind::Footer)),
                    noise_region("第 1 页", 80.0, RegionKind::Noise(NoiseKind::PageNumber)),
                    noise_region("章内散字", 60.0, RegionKind::Noise(NoiseKind::Seal)),
                    noise_region("脚注一行", 70.0, RegionKind::Footnote),
                ],
                source: PageSource::Ocr,
                dims: PageDims::default(),
            }],
        }
    }

    /// 默认渲染：家具/脚注零输出（与开关引入前逐字节相同）。
    #[test]
    fn furniture_hidden_by_default() {
        assert_eq!(render(&furniture_doc()), "正文");
        // DocIR::render() 同样默认关。
        assert_eq!(furniture_doc().render(), "正文");
    }

    /// 开关打开：段末注释行，按 y 升序（header → seal → footnote →
    /// page-number → footer），`Footnote` 用独立标签。
    #[test]
    fn furniture_emitted_as_comments_in_y_order() {
        let on = render_with_furniture(&furniture_doc(), true);
        assert_eq!(
            on,
            "正文\n\
             <!-- header: 页眉文本 -->\n\
             <!-- seal: 章内散字 -->\n\
             <!-- footnote: 脚注一行 -->\n\
             <!-- page-number: 第 1 页 -->\n\
             <!-- footer: 页脚文本 -->"
        );
    }

    /// 文本中的 `-->` 会提前终止 HTML 注释，输出前替换为 `->`。
    #[test]
    fn furniture_text_with_comment_terminator_is_sanitized() {
        let doc = DocIR {
            pages: vec![PageIR {
                page_no: 0,
                regions: vec![noise_region("a --> b", 0.0, RegionKind::Noise(NoiseKind::Header))],
                source: PageSource::Ocr,
                dims: PageDims::default(),
            }],
        };
        assert!(render_with_furniture(&doc, true).contains("<!-- header: a -> b -->"));
    }

    /// 占位变体（Image/Code/Formula/Aside）：零消费——producer 未产，
    /// 即便有人手工构造也不应出现在任何输出里。`Index` 已非占位（#10 INDEX
    /// 票有 producer），改由 [`index_entry_renders_as_list_item`] 单独钉住。
    #[test]
    fn placeholder_variants_never_render() {
        let doc = DocIR {
            pages: vec![PageIR {
                page_no: 0,
                regions: vec![
                    Region::new(0.0, 1.0, 0.0, 1.0, "图").with_kind(RegionKind::Image),
                    Region::new(0.0, 1.0, 0.0, 1.0, "码").with_kind(RegionKind::Code),
                    Region::new(0.0, 1.0, 0.0, 1.0, "式").with_kind(RegionKind::Formula),
                    Region::new(0.0, 1.0, 0.0, 1.0, "注").with_kind(RegionKind::Aside),
                ],
                source: PageSource::Ocr,
                dims: PageDims::default(),
            }],
        };
        assert_eq!(render(&doc), "");
        assert_eq!(render_with_furniture(&doc, true), "");
    }

    /// #10 INDEX：目次条目行渲染为 `- ` 列表项（MinerU v1 同形态），且与相邻
    /// 正文块之间有空行——否则 GFM 会把前一段吞进列表项。
    #[test]
    fn index_entry_renders_as_list_item() {
        let doc = DocIR {
            pages: vec![PageIR {
                page_no: 0,
                regions: vec![
                    Region::new(0.0, 1.0, 0.0, 1.0, "目 次").with_heading_level(Some(2)),
                    Region::new(0.0, 1.0, 0.0, 1.0, "前言…………IV").with_kind(RegionKind::Index),
                    Region::new(0.0, 1.0, 0.0, 1.0, "引言…………V").with_kind(RegionKind::Index),
                    Region::new(0.0, 1.0, 0.0, 1.0, "正文第一段。"),
                ],
                source: PageSource::TextLayerPdf,
                dims: PageDims::default(),
            }],
        };
        assert_eq!(
            render(&doc),
            "## 目 次\n\n- 前言…………IV\n- 引言…………V\n\n正文第一段。"
        );
    }
}
