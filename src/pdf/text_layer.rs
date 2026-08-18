//! PDF 文字层提取管线：pdf-inspector 提取 TextItem → 列感知拆行 → 阅读顺序还原
//! → 组装 Markdown。含坏字体检测（浅检/深检两级）、跨页"页面家具/水印"剔除、
//! 表格候选启发式 + 末页探针 + 版面 OCR 确认、文字层网格表重建等。
//! OCR 通路（`ocr_engine`）与渲染（`render`）留在 `mod.rs`。

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use crate::docir::{DocIR, PageSource};
use crate::fallback::{self, FallbackSignal};
use crate::region::{Region, RegionKind};
use crate::table_grid::{self};
use crate::{ConvertRequest, Result, gfm_adapter, reading_order};

/// garbled 检测常量：最多扫描前 4000 个 TextItem；字符总数须 >50，且
/// 坏字符占比 >=20%（bad * 100 >= total * 20）才判定为乱码
/// （阈值常量集中定义于 `text_health::GARBLED_MIN_TOTAL_CHARS` /
/// `GARBLED_BAD_PERCENT_THRESHOLD`，PDF/OFD 共用）。
const GARBLED_MAX_ITEMS: usize = 4000;

/// 宽间隙阈值（页宽比例）：行内相邻 item 的 gap > 1% 页宽视为列间隙。
/// 三处共用同一口径——双列拆行的 x 段分裂、列判定（`clustered_row_split`）、
/// 表格候选启发式（`page_has_tabular_rows`，行被拆成 >=3 段判为疑似表格）。
const MIN_GAP_FRACTION: f32 = 0.01;

/// 列间隙聚类容差（页宽比例）：候选 gap 中点彼此相差 <=2% 页宽归为同一簇；
/// 主簇 >=3 行才确认为全局列间隙（双列页 gutter 每行同一 x，聚成主簇；
/// 封面/标题的字母间距是单行现象，聚不到 3 行 → 不拆）。
const SPLIT_CLUSTER_TOL_FRACTION: f32 = 0.02;

/// 列间隙最小宽度（页宽比例）：候选 gap 须 >=3% 页宽才算列间隙。
/// 与 `reading_order::detect_column_split` 的 3% 口径一致。1% 的 `MIN_GAP_FRACTION`
/// 会把列表项（`a) `、`b) ` 等编号与正文的小间隙 ~1%）误判为列间隙，单栏页
/// 聚出假 gutter（9001c 文字版 4.1/4.2 正文缺行根因）。
const COL_GUTTER_GAP_FRACTION: f32 = 0.03;

/// P1.6：本通路所有"文字层不可用"出口的统一走查——信号供集中决策表
/// [`fallback::decide`] 裁决（PDF 粒度=文档级）。判回退（`OcrDoc`）→ `Ok(None)`，
/// 调用方整文档转 OCR；判文字层（当前决策表对这些信号恒回退，见 fallback.rs
/// 决策表单测）→ 本出口已无正文可产，返回空 `Some("")` 而非半途残稿。
fn no_text_layer(signal: FallbackSignal) -> Result<Option<String>> {
    if fallback::decide(std::slice::from_ref(&signal), fallback::Scope::Doc).is_ocr() {
        return Ok(None);
    }
    Ok(Some(String::new()))
}

/// 文字层 Markdown（P1.8 拆阶段）：提取 → 乱码防护 → 家具剔除 → 行组 →
/// 表格确认 → DocIR 装配。
///
/// 返回 `None` 表示无可用文字层（扫描件/提取失败），调用方回退 OCR。
///
/// `pub(crate)`：ADR-0005 候选 2 批处理预分流调用——`BatchConverter` 先逐 doc 试文字层，
/// 命中（Some）即快速路径出结果；未命中（None）的图片型 PDF 收集到 `ocr_paths`
/// 进跨文档 `PagePipeline`，与本函数解耦。
pub(crate) fn text_layer_markdown(path: &Path, opts: &ConvertRequest) -> Result<Option<String>> {
    let items = extract_text_items(path)?;
    // 图片型/扫描件（过滤后 items 空）直接回落 OCR，跳过开销大的 garbled 预检。
    if items.is_empty() {
        // P1.6：空文字层信号（图片型/扫描件）→ 集中决策表裁决（文档级）。
        return no_text_layer(FallbackSignal::EmptyTextLayer);
    }
    if is_garbled_doc(path, &items) {
        return Ok(None);
    }
    let by_page = strip_furniture(items);
    // 扫描件防护：文字层仅有页眉/页码等零星重复文本时，家具过滤可能删光全部 →
    // by_page 为空 → 无可用文字层，回落 OCR（而非 panic）。
    if by_page.is_empty() {
        // P1.6：家具剔除后空层信号 → 集中决策表裁决（文档级）。
        return no_text_layer(FallbackSignal::EmptyTextLayer);
    }
    let (lines_by_page, page_w, page_h) = build_line_groups(&by_page);
    // P0-2：不 unwrap——防御式回落（无文字层 → 整文档 OCR）而非 panic。
    // P1.6：回退裁决走集中决策表（空层信号，文档级）。
    let last_page = match by_page.keys().next_back() {
        Some(&p) => p,
        None => return no_text_layer(FallbackSignal::EmptyTextLayer),
    };
    let table_out = confirm_table_pages(path, opts, &by_page, &lines_by_page, &page_w);
    let last_table_md = probe_last_page_table(path, last_page, &page_w, &page_h);
    let md = assemble_docir(&by_page, &lines_by_page, &page_w, &table_out, last_page, last_table_md.as_deref());
    if md.is_empty() {
        // P1.6：装配输出为空（空层信号，文档级）→ 集中决策表裁决。
        no_text_layer(FallbackSignal::EmptyTextLayer)
    } else {
        Ok(Some(md))
    }
}

/// 文本提取：pdf-inspector 全文 TextItem + 过滤图片占位符。
///
/// ADR-0006：错误不再吞 `Ok(None)`——加密 PDF（Encrypted）、损坏 PDF
/// （InvalidStructure/Parse/NotAPdf）按 `PdfError` 分类返 `Err`，
/// batch 预分流阶段据此直接标错、不送 OCR（避免绕一大圈丢失分类）。
/// Io 错误（文件读不到等）同样返 Err，由调用方处理。
fn extract_text_items(path: &Path) -> Result<Vec<pdf_inspector::TextItem>> {
    let items = pdf_inspector::extract_text_with_positions(path).map_err(crate::error::from_pdf_error)?;
    // pdf-inspector 1.14+ 对图片对象返回 `[Image: ...]` 占位 TextItem（FormXob 引用等），
    // 非真实文字。过滤后判空——纯图片型 PDF（image.pdf/image_table.pdf）过滤后为空，
    // 回退 OCR，避免误判"有文字层"输出占位符。
    Ok(items
        .into_iter()
        .filter(|i| !i.text.trim_start().starts_with("[Image:"))
        .collect())
}

/// 坏字体（GID/编码损坏）两级防护（T12）：浅检在前、深检兜底。
///
/// - 浅检（廉价）：前 4000 个 TextItem 中替换符/私有区/控制符占比 >=20% → 乱码
///   （字符分类见 `looks_garbled`）。命中即返回，省下深检的 ~0.3s 全页 markdown
///   构建（健康文档白付的成本）。拉丁扩展乱码（如某些 GID 字体）此处检不出，
///   由深检兜住。
/// - 深检（兜底）：pdf-inspector 健壮检测器全文档抽取，统计被判
///   `suspected_garbled_text` 的页数，占比 >=20% 且至少 3 页 → 系统性坏字体。
///   健康文档即使有少量误报（如目录点线符的私有区字符，上海公报仅 2 页）也不触发。
///   注意：pdf-inspector 内部行分组有 bug（layout.rs:1270 空列集 then_some 立即
///   求值 panic），extract_pages_markdown 也会触发——catch_unwind 兜底，panic 视为
///   "无法预检"跳过。T11：path 版内部 `fs::read` 整读（500MB 文档重复 500MB 堆
///   峰值），用 mmap 版 `_mem` 零拷贝映射，与末页探针同思路。
///
/// 两级信号均经 [`fallback::decide`]（文档级）集中裁决。
fn is_garbled_doc(path: &Path, items: &[pdf_inspector::TextItem]) -> bool {
    // 浅检
    if looks_garbled(items)
        && fallback::decide(&[FallbackSignal::GarbledShallow], fallback::Scope::Doc).is_ocr()
    {
        return true;
    }
    // 深检
    let Some(pdf_bytes) = open_pdf_bytes(path) else {
        return false; // 文件不可读 → 无法预检，继续文字层
    };
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        pdf_inspector::extract_pages_markdown_mem(pdf_bytes.as_slice(), None)
    }));
    let Ok(Ok(extraction)) = caught else {
        return false; // panic/提取失败 → 无法预检，继续文字层
    };
    let total = extraction.pages.len();
    let garbled = extraction
        .ocr_reasons_by_page
        .iter()
        .filter(|r| {
            r.reasons
                .iter()
                .any(|s| s == pdf_inspector::OCR_REASON_SUSPECTED_GARBLED_TEXT)
        })
        .count();
    // 乱码页占比 >=20% 且至少 3 页 → 判定系统性坏字体，回退 OCR。
    garbled >= 3
        && total > 0
        && garbled * 100 >= total * 20
        && fallback::decide(&[FallbackSignal::GarbledDeep], fallback::Scope::Doc).is_ocr()
}

/// 跨页重复"页面家具/水印"剔除并按页分组（`TextItem.page` 1 起始，页序升序）。
///
/// 同文本 + 同归一化位置（x 中心、y 各 1% 箱）出现在 >=pages_needed 个不同页 →
/// 页眉/页脚/水印，剔除后再做行分组。单页/页数不足时 pages_needed > total_pages →
/// 零剔除（零误杀）。
fn strip_furniture(items: Vec<pdf_inspector::TextItem>) -> BTreeMap<u32, Vec<pdf_inspector::TextItem>> {
    let total_pages = items.iter().map(|i| i.page).max().unwrap_or(0) as usize;
    let pages_needed = std::cmp::max(3usize, ((total_pages as f32) * 0.6).ceil() as usize);
    let furniture = is_repeated_furniture(&items, pages_needed, total_pages);
    let items: Vec<pdf_inspector::TextItem> = items
        .into_iter()
        .filter(|i| !furniture.contains(&(i.page, i.x.to_bits(), i.y.to_bits(), i.text.clone())))
        .collect();
    let mut by_page: BTreeMap<u32, Vec<pdf_inspector::TextItem>> = BTreeMap::new();
    for item in items {
        by_page.entry(item.page).or_default().push(item);
    }
    by_page
}

/// 每页行组预构建（列检测与表格启发式共用一次）+ 每页近似宽/高缓存。
///
/// 空页防护：pdf-inspector 的 group_into_lines 对空 items 会 panic
/// （layout.rs index out of bounds）。某页无提取文本（如扫描件夹杂页）时跳过
/// 行分组，该页后续按无行处理（不崩、回落/空输出）。
/// pdf-inspector 自身 bug 兜底：group_into_lines 内部 `(len==2).then_some(columns[0])`
/// 对空列集立即求值 → index panic（layout.rs:1270）。正常页不触发、零开销；
/// 异常页回落空行组。
fn build_line_groups(
    by_page: &BTreeMap<u32, Vec<pdf_inspector::TextItem>>,
) -> (
    BTreeMap<u32, Vec<pdf_inspector::extractor::TextLine>>,
    BTreeMap<u32, f32>,
    BTreeMap<u32, f32>,
) {
    let mut lines_by_page: BTreeMap<u32, Vec<pdf_inspector::extractor::TextLine>> = BTreeMap::new();
    let mut page_w: BTreeMap<u32, f32> = BTreeMap::new();
    let mut page_h: BTreeMap<u32, f32> = BTreeMap::new();
    for (&page, page_items) in by_page {
        let mut w = 0.0_f32;
        let mut h = 0.0_f32;
        for i in page_items {
            w = w.max(i.x + i.width);
            h = h.max(i.y + i.height);
        }
        page_w.insert(page, w);
        page_h.insert(page, h);
        let lines = if page_items.is_empty() {
            Vec::new()
        } else {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                pdf_inspector::extractor::group_into_lines_preserving_all_text(page_items.clone())
            }))
            .unwrap_or_default()
        };
        lines_by_page.insert(page, lines);
    }
    (lines_by_page, page_w, page_h)
}

/// T2-B/R1：可疑表格页确认（单一信号，最终确认靠版面 OCR）。
///
/// 信号（唯一来源）：文字层启发式——某页 >=3 行各自被宽间隙拆成 >=3 个 x
/// 分离段。保守：双列正文每行仅 2 段（1 条 gutter），不会误报；真表格/目录行
/// 多为多列。有证据才渲染，避免为"可能有表"的猜测付整页版面 OCR。
/// 末页表格不走本集合：由 `probe_last_page_table` 独立兜底（pdf-inspector 表格
/// 提取，无渲染开销）；首页不强制入集（Ticket B：无证据召回，代价是每文档
/// 1~2 次整页渲染 + 版面 OCR）。
///
/// 懒渲染（仅可疑集非空才做）+ 批量版面 OCR（用 `opts.ocr.layout`，默认 Doc：
/// 含 table 类，能识别封面/版权栏等；Table 版面只标 Table，漏检严重）。确认
/// 有 `LayoutElementType::Table` 的页 → 单页 gfm（行 + `<table>`）；未确认页 →
/// 回落文字层（防误报，R2 gfm 过滤仍生效）。渲染/OCR 任一环节失败 → 该页
/// 回落，不炸文档。
fn confirm_table_pages(
    path: &Path,
    opts: &ConvertRequest,
    by_page: &BTreeMap<u32, Vec<pdf_inspector::TextItem>>,
    lines_by_page: &BTreeMap<u32, Vec<pdf_inspector::extractor::TextLine>>,
    page_w: &BTreeMap<u32, f32>,
) -> BTreeMap<u32, String> {
    let mut suspicious: BTreeSet<u32> = BTreeSet::new();
    for (&page, lines) in lines_by_page {
        let Some(&w) = page_w.get(&page) else { continue };
        if page_has_tabular_rows(lines, w) {
            suspicious.insert(page);
        }
    }
    let mut table_out: BTreeMap<u32, String> = BTreeMap::new();
    if suspicious.is_empty() {
        return table_out;
    }
    // T07 懒惰渲染：只渲 `suspicious` 子集（0 基准 pdfium 页号 = p-1），
    // 避免 52p 文档全量渲 52 页只 OCR 3 页的内存浪费。`suspicious` 是
    // BTreeSet 升序，to_render 与 ocr_pages 同迭代序/同谓词 → 输出锁步。
    let to_render: Vec<u32> = suspicious
        .iter()
        .filter(|&&p| by_page.contains_key(&p))
        .map(|&p| p - 1)
        .collect();
    let ocr_pages: Vec<u32> = suspicious
        .iter()
        .filter(|&&p| by_page.contains_key(&p))
        .copied()
        .collect();
    if !to_render.is_empty()
        && let Ok(imgs) = super::render::render_pdf_pages(path, opts.render.dpi, &to_render)
        && !imgs.is_empty()
        && let Ok(results) = crate::ocr_engine::ocr_images(
            imgs,
            opts.ocr.tier,
            opts.ocr.layout,
            opts.parallel.page_parallel,
            None,
        )
    {
        for (page, res) in ocr_pages.into_iter().zip(results) {
            let has_table = res
                .layout_elements
                .iter()
                .any(|e| e.element_type == oar_ocr::domain::structure::LayoutElementType::Table);
            if has_table {
                table_out.insert(page, gfm_adapter::to_markdown(std::slice::from_ref(&res)));
            }
        }
    }
    table_out
}

/// 输出装配（P1.5 DocIR producer）：文字层网格表（免 OCR、跨页合并）+
/// OCR 确认表 + 普通行，页序混排。跨页 Grid 合并由
/// `docir::passes::cross_page_table` 统一承担，渲染由 `docir::render` 统一
/// 消费（与旧 emitter 通路字节一致，golden 守护）。
fn assemble_docir(
    by_page: &BTreeMap<u32, Vec<pdf_inspector::TextItem>>,
    lines_by_page: &BTreeMap<u32, Vec<pdf_inspector::extractor::TextLine>>,
    page_w_map: &BTreeMap<u32, f32>,
    table_out: &BTreeMap<u32, String>,
    last_page: u32,
    last_table_md: Option<&str>,
) -> String {
    let mut doc = DocIR::default();
    for (page, page_items) in by_page.iter() {
        let (Some(&page_w), Some(full_lines)) = (page_w_map.get(page), lines_by_page.get(page))
        else {
            continue; // 不变量：两表均由 build_line_groups 从 by_page 构建，键恒一致
        };

        // 1) 文字层网格表格（快速、免 OCR）：Grid 区块（同列续接/换列定格由 pass 承担）
        let blocks: Vec<Region> = page_items
            .iter()
            .map(|i| Region::from_top_left(i.x, -i.y, i.width, i.height, i.text.clone()))
            .collect();
        if let Some(grid) = table_grid::reconstruct_table_grid(&blocks, page_w) {
            doc.push_page(
                *page,
                PageSource::TextLayerPdf,
                vec![Region::new(0.0, 0.0, 0.0, 0.0, String::new())
                    .with_kind(RegionKind::Grid(grid))],
            );
            continue;
        }

        // 2) 版面 OCR 确认的表格页：成品块（行 + <table>，producer 已含精确格式）
        if let Some(ocr_md) = table_out.get(page) {
            doc.push_page(
                *page,
                PageSource::TextLayerPdf,
                vec![Region::new(0.0, 0.0, 0.0, 0.0, ocr_md.clone())
                    .with_kind(RegionKind::PreRendered)],
            );
            continue;
        }

        // 3) 普通页：文字层行（Body 区块）+ 末页表格探针兜底（成品块）
        let mut out = build_body_regions(full_lines, *page, page_w);
        // R3 兜底：末页布局未确认但 pdf-inspector 探针提取到表格（版权栏等小表格）
        // → 文字层行后追加管道表，保证表格信息不丢（保留正文行，仅追加结构）。
        if *page == last_page
            && let Some(tbl) = last_table_md
        {
            out.push(
                Region::new(0.0, 0.0, 0.0, 0.0, format!("\n{tbl}\n"))
                    .with_kind(RegionKind::PreRendered),
            );
        }
        doc.push_page(*page, PageSource::TextLayerPdf, out);
    }
    crate::docir::passes::cross_page_table::run(&mut doc);
    doc.render()
}

/// 普通页正文行构建：列间隙检测 + 双列拆行 → 阅读顺序 → 标题前缀 → Body 区块。
fn build_body_regions(
    full_lines: &[pdf_inspector::extractor::TextLine],
    page: u32,
    page_w: f32,
) -> Vec<Region> {
    // 列间隙检测：行级候选间隙聚类。封面/标题的字母间距是单行现象、每行
    // split_x 各不相同，聚类不到 >=3 行；双列正文的 gutter 在每行同一 x 处
    // 重复出现，聚成主簇 → 只拆这些行，标题行保持整行。
    let split = clustered_row_split(full_lines, page_w);

    let mut regions: Vec<Region> = Vec::new();
    for line in full_lines {
        let mut sorted = line.items.clone();
        sorted.sort_by(|a, b| a.x.total_cmp(&b.x));
        // 找最接近全局 split 的内部间隙（若存在且够宽），从那里拆成左右两段
        let mut seg: Vec<pdf_inspector::TextItem> = Vec::new();
        if let Some(s) = split {
            let mut split_idx: Option<usize> = None;
            let mut best_dist = f32::INFINITY;
            for i in 1..sorted.len() {
                let gap = sorted[i].x - (sorted[i - 1].x + sorted[i - 1].width);
                if gap > MIN_GAP_FRACTION * page_w {
                    let mid = (sorted[i - 1].x + sorted[i - 1].width + sorted[i].x) / 2.0;
                    let d = (mid - s).abs();
                    if d < best_dist {
                        best_dist = d;
                        split_idx = Some(i);
                    }
                }
            }
            if let Some(idx) = split_idx {
                for item in sorted.drain(..idx) {
                    seg.push(item);
                }
                push_line_region(&seg, line, page, &mut regions);
                seg = sorted;
                push_line_region(&seg, line, page, &mut regions);
                continue;
            }
        }
        seg = sorted;
        push_line_region(&seg, line, page, &mut regions);
    }

    // B3-T：标题前缀注入统一于 `text_health::apply_title_prefixes`
    // （空 hints + numbering=true，纯编号启发式，与 OFD 文字层同口径）。
    // 标题（# 开头）前后空行语义由 docir 渲染层统一施加（TextLayerPdf 分支）。
    crate::text_health::apply_title_prefixes(
        &reading_order::postprocess_lines(reading_order::order_text_regions(&regions)),
        &[],
        true,
    )
    .into_iter()
    .map(|l| Region::new(0.0, 0.0, 0.0, 0.0, l))
    .collect()
}

/// 从每行内找出"列间隙"候选（gap 中点），按 x 聚类；主簇 >=3 行才返回全局 split_x。
///
/// 双列页：每行的 gutter 都在同一 x → 聚成主簇。封面大标题字母间距大但每行
/// split_x 不同/行数少 → 主簇不足 3 → 返回 None，行保持整行。
fn clustered_row_split(lines: &[pdf_inspector::extractor::TextLine], page_w: f32) -> Option<f32> {
    let min_gap = COL_GUTTER_GAP_FRACTION * page_w;
    let tol = SPLIT_CLUSTER_TOL_FRACTION * page_w;
    let mut candidates: Vec<f32> = Vec::new();
    for line in lines {
        let mut sorted = line.items.clone();
        sorted.sort_by(|a, b| a.x.total_cmp(&b.x));
        let mut best_gap = min_gap;
        let mut best_mid: Option<f32> = None;
        for i in 1..sorted.len() {
            let gap = sorted[i].x - (sorted[i - 1].x + sorted[i - 1].width);
            if gap > best_gap {
                best_gap = gap;
                best_mid = Some((sorted[i - 1].x + sorted[i - 1].width + sorted[i].x) / 2.0);
            }
        }
        if let Some(mid) = best_mid {
            candidates.push(mid);
        }
    }
    if candidates.len() < 3 {
        return None;
    }
    candidates.sort_by(|a, b| a.total_cmp(b));
    let mut clusters: Vec<Vec<f32>> = Vec::new();
    for c in candidates {
        if let Some(last) = clusters.last_mut() {
            if (last[0] - c).abs() <= tol {
                last.push(c);
            } else {
                clusters.push(vec![c]);
            }
        } else {
            clusters.push(vec![c]);
        }
    }
    let dominant = clusters.iter().max_by_key(|c| c.len())?;
    (dominant.len() >= 3).then(|| dominant.iter().sum::<f32>() / dominant.len() as f32)
}

/// 表格候选启发式（R1 信号2）：某页是否"疑似表格"。
///
/// 规则：存在 >=3 行，每行被宽间隙（>1% 页宽，与列检测同口径）拆成 >=3 个
/// x 分离段 → 疑似表格。保守设计：双列正文每行只有 1 条 gutter → 2 段，
/// 永远够不到 3 段；封面/标题的字母间距是单行现象，行数不足 3。真表格行
/// 多为多列（>=3 段）且跨多行对齐 → 命中。误报也无妨：命中页会走 Table
/// 版面 OCR，最终以 `LayoutElementType::Table` 确认，未确认即回落文字层。
fn page_has_tabular_rows(lines: &[pdf_inspector::extractor::TextLine], page_w: f32) -> bool {
    let min_gap = MIN_GAP_FRACTION * page_w;
    let mut multi_seg_rows = 0usize;
    for line in lines {
        let mut sorted = line.items.clone();
        sorted.sort_by(|a, b| a.x.total_cmp(&b.x));
        let mut segs = 1usize;
        for i in 1..sorted.len() {
            let gap = sorted[i].x - (sorted[i - 1].x + sorted[i - 1].width);
            if gap > min_gap {
                segs += 1;
            }
        }
        if segs >= 3 {
            multi_seg_rows += 1;
            if multi_seg_rows >= 3 {
                return true;
            }
        }
    }
    false
}

/// R3 末页探针：在末页全页区域内跑一次 pdf-inspector 表格提取。
///
/// 布局模型对页脚版权栏这类小表格常漏检；此探针用 pdf-inspector 的
/// rect→line→启发式检测兜底，命中返回管道表 markdown。区域坐标为
/// PDF 点、top-left 原点（`extract_tables_in_regions_mem` 约定），
/// 宽/高加 40pt 余量防边缘裁剪。任何失败/空结果 → `None`，不影响主流程。
///
/// T11：上游只有 `&[u8]` 接口，原实现 `fs::read` 会把整个 PDF 载入堆
/// （500MB 文件 → 500MB 峰值）。改为 mmap 只读映射，页面按需换入、
/// 不占堆；映射失败（无 mmap 的文件系统等）回落一次性整读。
fn probe_last_page_table(
    path: &Path,
    last_page: u32,
    page_w: &BTreeMap<u32, f32>,
    page_h: &BTreeMap<u32, f32>,
) -> Option<String> {
    let pdf_bytes = open_pdf_bytes(path)?;
    let w = page_w.get(&last_page).copied().unwrap_or(595.0);
    let h = page_h.get(&last_page).copied().unwrap_or(842.0);
    let regions = [(
        last_page.saturating_sub(1),
        vec![[0.0, 0.0, w + 40.0, h + 40.0]],
    )];
    let results =
        pdf_inspector::extract_tables_in_regions_mem(pdf_bytes.as_slice(), &regions).ok()?;
    let md = results.into_iter().next()?.regions.into_iter().next()?.text;
    let md = md.trim();
    (!md.is_empty()).then(|| md.to_string())
}

/// 只读打开 PDF 字节的载体：优先 mmap（零堆拷贝，T11），失败回落整读。
enum PdfBytes {
    Mapped(memmap2::Mmap),
    Owned(Vec<u8>),
}
impl PdfBytes {
    fn as_slice(&self) -> &[u8] {
        match self {
            PdfBytes::Mapped(m) => m.as_ref(),
            PdfBytes::Owned(v) => v,
        }
    }
}

/// 只读打开 PDF 字节：优先 mmap，映射失败（无 mmap 的文件系统等）回落整读。
/// 返回 `None` 表示文件不可读（调用方据此跳过相应兜底逻辑）。
fn open_pdf_bytes(path: &Path) -> Option<PdfBytes> {
    if let Ok(f) = std::fs::File::open(path) {
        // SAFETY: 只读映射输入 PDF。若外部进程在转换期间截断/改写该文件，对
        // 已映射页的访问可能触发 SIGBUS（mmap 语义，进程终止）或读到不一致
        // 字节——这是 mmap 读者的固有风险，非本工具引入；此处仅用于解析，
        // 文件被并发改写属调用方环境问题。Linux 下映射不依赖 fd 存活。
        if let Ok(m) = unsafe { memmap2::Mmap::map(&f) } {
            return Some(PdfBytes::Mapped(m));
        }
    }
    std::fs::read(path).ok().map(PdfBytes::Owned)
}

/// 坏字体乱码检测：前 4000 个 TextItem 中替换符 `\u{FFFD}`、私有区
/// (U+E000..=U+F8FF)、控制字符占比达 20% 且字符总数 >50 → 判定乱码，
/// 文字层应回退 OCR。字符分类逻辑收敛于 `text_health`。
fn looks_garbled(items: &[pdf_inspector::TextItem]) -> bool {
    let chars = items
        .iter()
        .take(GARBLED_MAX_ITEMS)
        .flat_map(|it| it.text.chars());
    crate::text_health::has_garbled_chars(
        chars,
        crate::text_health::GARBLED_MIN_TOTAL_CHARS,
        crate::text_health::GARBLED_BAD_PERCENT_THRESHOLD,
    )
}

/// 跨页重复"页面家具/水印"检测：返回需剔除的 TextItem 签名集合
/// `(page, x_bits, y_bits, text)`（x/y 用 `f32::to_bits()` 存——`f32` 不满足
/// `Eq`/`Hash`，不能直接作为 `HashSet` 元素；位模式保精度、去重精确）。
///
/// 判定规则：trim 后文本相同，且在 >= `pages_needed` 个不同页面出现在相似
/// 归一化位置（x 中心、y 各自 1% 箱内）→ 页眉/页脚/居中/斜向水印等重复家具。
///
/// 归一化：每页页宽≈max(x+width)、页高≈max(y+height)（PDF y 原点左下）；
/// x_norm=(x+width/2)/页宽，y_norm=(y+height/2)/页高，再取 1% 箱
/// `(x_norm*100) as i32, (y_norm*100) as i32`。
///
/// `page_total < pages_needed`（如单页文档）直接返回空集 → 零误杀。
fn is_repeated_furniture(
    items: &[pdf_inspector::TextItem],
    pages_needed: usize,
    page_total: usize,
) -> HashSet<(u32, u32, u32, String)> {
    if items.is_empty() || page_total < pages_needed {
        return HashSet::new();
    }
    // 每页近似页宽/页高
    let mut page_max_x: BTreeMap<u32, f32> = BTreeMap::new();
    let mut page_max_y: BTreeMap<u32, f32> = BTreeMap::new();
    for it in items {
        let xr = it.x + it.width;
        let yr = it.y + it.height;
        page_max_x
            .entry(it.page)
            .and_modify(|m| *m = m.max(xr))
            .or_insert(xr);
        page_max_y
            .entry(it.page)
            .and_modify(|m| *m = m.max(yr))
            .or_insert(yr);
    }
    let bin = |it: &pdf_inspector::TextItem| -> (i32, i32) {
        let mx = page_max_x.get(&it.page).copied().unwrap_or(1.0).max(1.0);
        let my = page_max_y.get(&it.page).copied().unwrap_or(1.0).max(1.0);
        let x_norm = (it.x + it.width / 2.0) / mx;
        let y_norm = (it.y + it.height / 2.0) / my;
        ((x_norm * 100.0) as i32, (y_norm * 100.0) as i32)
    };
    // key = (trimmed_text, x_bin, y_bin) → 出现过的不同页集合
    let mut key_pages: BTreeMap<(String, i32, i32), BTreeSet<u32>> = BTreeMap::new();
    for it in items {
        let text = it.text.trim();
        if text.is_empty() {
            continue;
        }
        let (xb, yb) = bin(it);
        key_pages
            .entry((text.to_string(), xb, yb))
            .or_default()
            .insert(it.page);
    }
    let furniture: HashSet<(String, i32, i32)> = key_pages
        .into_iter()
        .filter(|(_, pages)| pages.len() >= pages_needed)
        .map(|(k, _)| k)
        .collect();
    items
        .iter()
        .filter(|it| {
            let (xb, yb) = bin(it);
            furniture.contains(&(it.text.trim().to_string(), xb, yb))
        })
        .map(|it| (it.page, it.x.to_bits(), it.y.to_bits(), it.text.clone()))
        .collect()
}

/// 把一段（列内）TextItem 组行并转为 region。复用 pdf-inspector 的文本拼接。
fn push_line_region(
    seg: &[pdf_inspector::TextItem],
    template: &pdf_inspector::extractor::TextLine,
    page: u32,
    regions: &mut Vec<Region>,
) {
    if seg.is_empty() {
        return;
    }
    let line = pdf_inspector::extractor::TextLine {
        items: seg.to_vec(),
        y: template.y,
        page,
        adaptive_threshold: template.adaptive_threshold,
    };
    let text = line.text().trim().to_string();
    if text.is_empty() {
        return;
    }
    let mut x_min = f32::INFINITY;
    let mut x_max = f32::NEG_INFINITY;
    let mut y_max_pdf = f32::NEG_INFINITY;
    for item in &line.items {
        x_min = x_min.min(item.x);
        x_max = x_max.max(item.x + item.width);
        y_max_pdf = y_max_pdf.max(item.y);
    }
    // PDF 坐标原点左下（y 大=靠上）。reading_order 约定 y 越小越靠上，翻转：-y。
    let y_flip = -line.y;
    regions.push(Region::new(
        x_min,
        x_max,
        y_flip,
        y_flip + (y_max_pdf - line.y).max(1.0),
        text,
    ));
}

#[cfg(test)]
mod tests {
    use super::{clustered_row_split, is_repeated_furniture, looks_garbled};
    use pdf_inspector::TextItem;
    use pdf_inspector::extractor::TextLine;

    const PAGE_W: f32 = 595.0; // A4 宽（pt）

    fn ti(text: &str, x: f32, width: f32) -> TextItem {
        TextItem {
            text: text.to_string(),
            x,
            y: 0.0,
            width,
            height: 10.0,
            font: "test".into(),
            font_size: 10.0,
            page: 1,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: false,
            item_type: Default::default(),
            mcid: None,
        }
    }

    /// 带页面/坐标的 TextItem 构造（家具检测测试用）。
    fn tif(text: &str, x: f32, y: f32, w: f32, h: f32, page: u32) -> TextItem {
        TextItem {
            text: text.to_string(),
            x,
            y,
            width: w,
            height: h,
            font: "test".into(),
            font_size: 10.0,
            page,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: false,
            item_type: Default::default(),
            mcid: None,
        }
    }

    fn tl(items: Vec<TextItem>) -> TextLine {
        TextLine {
            items,
            y: 0.0,
            page: 1,
            adaptive_threshold: 0.0,
        }
    }

    /// 双列正文：左列 x≈50..65，右列 x≈340..355，gutter 中点 ≈202.5。
    /// 4 行重复同一 gutter → 主簇 >=3 → 返回 ≈202.5。
    #[test]
    fn two_column_rows_return_gutter_midpoint() {
        let lines: Vec<TextLine> = (0..4)
            .map(|_| {
                tl(vec![
                    ti("左", 50.0, 5.0),
                    ti("列", 55.0, 5.0),
                    ti("文", 60.0, 5.0),
                    ti("右", 340.0, 5.0),
                    ti("列", 345.0, 5.0),
                    ti("文", 350.0, 5.0),
                ])
            })
            .collect();
        let split = clustered_row_split(&lines, PAGE_W).expect("应检测到双列 gutter");
        assert!((split - 202.5).abs() < 1e-3, "split={split}");
    }

    /// 标题行字母间距大（每行 split_x 不同）+ 两行各自不同的大间隙 → 每簇仅 1 行，
    /// 无 >=3 主簇 → None，标题行保持整行。
    #[test]
    fn scattered_gaps_return_none() {
        // 标题行：等宽字母间距 20（> min_gap 5.95），只产生 1 个候选
        let title = tl(vec![
            ti("T", 50.0, 10.0),
            ti("I", 80.0, 10.0),
            ti("T", 110.0, 10.0),
            ti("L", 140.0, 10.0),
        ]);
        // 另两行：间隙不同 x 处，各自形成独立簇
        let row2 = tl(vec![ti("a", 50.0, 20.0), ti("b", 250.0, 20.0)]);
        let row3 = tl(vec![ti("c", 60.0, 20.0), ti("d", 300.0, 20.0)]);
        let split = clustered_row_split(&[title, row2, row3], PAGE_W);
        assert_eq!(split, None);
    }

    /// 候选行 <3 → None。
    #[test]
    fn fewer_than_three_candidate_rows_return_none() {
        let lines: Vec<TextLine> = (0..2)
            .map(|_| {
                tl(vec![
                    ti("左", 50.0, 5.0),
                    ti("列", 55.0, 5.0),
                    ti("右", 340.0, 5.0),
                    ti("列", 345.0, 5.0),
                ])
            })
            .collect();
        assert_eq!(clustered_row_split(&lines, PAGE_W), None);
    }

    /// 大量替换符 \u{FFFD}（占比 50% > 20%）→ 乱码。
    #[test]
    fn many_replacement_chars_is_garbled() {
        let items = vec![ti(
            &format!("{}{}", "a".repeat(30), "\u{FFFD}".repeat(30)),
            0.0,
            10.0,
        )];
        assert!(looks_garbled(&items));
    }

    /// 正常 CJK/Latin 文本 → 非乱码。
    #[test]
    fn normal_text_is_not_garbled() {
        let items = vec![ti(
            "你好，世界 Hello World, this is a normal sentence.",
            0.0,
            10.0,
        )];
        assert!(!looks_garbled(&items));
    }

    /// 同文本 + 同归一化位置出现在 5/6 页（pages_needed=4）→ 判为家具，签名剔除。
    /// 正文每页不同（真实正文如此），不受影响。
    #[test]
    fn same_text_same_position_on_most_pages_is_furniture() {
        let mut items: Vec<TextItem> = Vec::new();
        for page in 1..=6u32 {
            // 正文每页不同，保证各页 page_max 一致
            items.push(tif(
                &format!("正文内容第{page}页"),
                100.0,
                400.0,
                200.0,
                10.0,
                page,
            ));
            if page <= 5 {
                // 页眉：页 1..5 同一位置
                items.push(tif(
                    "上海市人民政府公报 2025·1",
                    200.0,
                    800.0,
                    100.0,
                    10.0,
                    page,
                ));
            }
        }
        let drop = is_repeated_furniture(&items, 4, 6);
        // 5 个页眉全部命中
        assert_eq!(drop.len(), 5, "drop={drop:?}");
        for page in 1..=5u32 {
            assert!(drop.contains(&(
                page,
                200.0f32.to_bits(),
                800.0f32.to_bits(),
                "上海市人民政府公报 2025·1".to_string()
            )));
        }
        // 正文不受影响
        for page in 1..=6u32 {
            assert!(!drop.contains(&(
                page,
                100.0f32.to_bits(),
                400.0f32.to_bits(),
                format!("正文内容第{page}页")
            )));
        }
    }

    /// 同文本但每页位置不同（x 超出 1% 箱）→ 每个 key 仅 1 页 → 非家具。
    #[test]
    fn same_text_different_position_per_page_not_furniture() {
        let mut items: Vec<TextItem> = Vec::new();
        for page in 1..=6u32 {
            items.push(tif(
                &format!("正文内容第{page}页"),
                100.0,
                400.0,
                200.0,
                10.0,
                page,
            ));
            items.push(tif(
                "WATERMARK",
                50.0 + page as f32 * 100.0,
                800.0,
                30.0,
                10.0,
                page,
            ));
        }
        let drop = is_repeated_furniture(&items, 4, 6);
        assert!(drop.is_empty(), "drop={drop:?}");
    }

    /// 文本仅出现在 2 页 → 2 < pages_needed → 非家具；单页文档 → 恒空集。
    #[test]
    fn rare_text_and_single_page_never_filtered() {
        let mut items: Vec<TextItem> = Vec::new();
        for page in 1..=6u32 {
            items.push(tif(
                &format!("正文内容第{page}页"),
                100.0,
                400.0,
                200.0,
                10.0,
                page,
            ));
            if page <= 2 {
                items.push(tif("罕见脚注", 200.0, 50.0, 100.0, 10.0, page));
            }
        }
        assert!(is_repeated_furniture(&items, 4, 6).is_empty());
        // 单页文档：pages_needed=3 > total=1 → 空集
        let single = vec![tif("标题", 200.0, 800.0, 100.0, 10.0, 1)];
        assert!(is_repeated_furniture(&single, 3, 1).is_empty());
    }

    // ── 文字层表格网格重建 + 跨页合并 ──

    /// 编号启发式标题前缀（B3-T）：`一、总则`→`## `，`1.1 适用范围`→`### `；
    /// 带结束标点的正文不变；`第X章` 不被 `title_level` 识别 → 不变。
    #[test]
    fn title_prefixes_by_numbering_heuristic() {
        let lines: Vec<String> = vec![
            "一、总则".into(),
            "这是正文第一句。".into(),
            "1.1 适用范围".into(),
            "第二章 附则".into(),
        ];
        let out = crate::text_health::apply_title_prefixes(&lines, &[], true);
        assert_eq!(
            out,
            vec![
                "## 一、总则".to_string(),
                "这是正文第一句。".to_string(),
                "### 1.1 适用范围".to_string(),
                "第二章 附则".to_string(),
            ]
        );
    }

    /// 回归（9001c 文字版 4.1/4.2 正文缺行根因）：单栏页的列表项编号间隙
    /// （`a) `、`b) ` 等，~1% 页宽）不得被当成列间隙聚成假 gutter → 返回 None。
    /// 此前 `MIN_GAP_FRACTION=0.01` 会把 `a)/b)/c)` 与正文间的小间隙判为列间隙，
    /// 多处编号行聚成主簇 → 误判双列 → 正文被拆/颠倒/丢失。
    #[test]
    fn list_label_gaps_do_not_form_false_column() {
        // 单栏：5 个列表项行（编号与正文 gap≈1%），其余为通栏正文行。
        // 通栏正文行无 >3% 间隙 → 主簇候选仅来自列表项 → 3% 阈值下全部被过滤。
        let list_rows: Vec<TextLine> = (0..5)
            .map(|_| {
                tl(vec![
                    ti("a)", 75.0, 12.0),                            // 编号
                    ti("与质量管理体系有关的相关方；", 91.0, 200.0), // 正文，gap≈4pt
                ])
            })
            .collect();
        let body_rows: Vec<TextLine> = (0..10)
            .map(|_| tl(vec![ti("组织应确定与所承担装备任务相关的法律法规、标准、使用需求、保障条件等影响因素。", 75.0, 400.0)]))
            .collect();
        // 通栏正文行无 gap；列表项 gap=(91-87)=4pt，占 595 的 0.67% < 3% → 非候选
        let mut lines = list_rows;
        lines.extend(body_rows);
        assert_eq!(clustered_row_split(&lines, PAGE_W), None);
    }
}
