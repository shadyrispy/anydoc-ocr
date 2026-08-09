//! PDF 通道：文字型走 pdf-inspector 提取 + 自建阅读顺序还原；图片型走 OCR 管线。
//!
//! 文字型不再用 `anydoc::to_markdown()`：其内部是 pdf-inspector 的"既有版式路径"
//! （朴素 y/x 排序），双列页面逐行交错，且 pdf-inspector 内置 reading_order 是
//! 图像锚定、证据门控的局部列流处理，纯文字双列页永不触发。改为直接调
//! `extract_text_with_positions()` 拿带坐标 TextItem → 复用公共模块
//! `reading_order`（与 OCR 通路同一算法）还原阅读顺序。
//!
//! 关键：pdf-inspector 的 `group_into_lines` 会把同一行的左右两列合并成一行
//! （先于列检测糊掉列边界），所以这里在检测到双列后**按 gutter 拆行**，把每行
//! 拆成列内独立行，再交给 `order_text_regions` 做左列全→右列全。
//!
//! T2-B/R1/R3：文字层通路对"含表格页"回退 OCR。不再单靠 pdf-inspector 的
//! `pages_with_tables`（弱：漏首页标题块、偶误报），改为可疑集 = ① 文字层启
// 发式（>=3 行各自拆成 >=3 个 x 分离段，双列正文每行仅 2 段不误报）；② 首页
//! 强制入集（公报封面标题块，布局模型可识别为表）；③ R3 末页探针（末页强制
//! 入集 + pdf-inspector 表格提取兜底）。可疑页整文档懒渲染一次后批量跑版面
//! OCR（用 `opts.ocr_layout`，默认 Doc 含 table 类，能识别封面/版权栏等），
//! 以 `LayoutElementType::Table` 确认后才输出 `<table>` HTML（MinerU 对齐：
//! 表格只出自识别模型，不来自文字层），未确认页回落文字层；页序混排保序，
//! OCR 失败回落该页文字层。`--pdf-force-ocr` 仍为整文档 OCR。
//!
//! T2-B：跨页重复的"页面家具/水印"（页眉/页脚/居中/斜向水印）在文字层按
//! "同文本 + 同归一化位置跨页重复达阈值"剔除，避免污染阅读顺序。
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use crate::{gfm_adapter, reading_order, timing::StageTimer, ConvertOptions, Result};

/// garbled 检测常量：最多扫描前 4000 个 TextItem；字符总数须 >50，且
/// 坏字符占比 >=20%（bad * 100 >= total * 20）才判定为乱码。
const GARBLED_MAX_ITEMS: usize = 4000;
const GARBLED_MIN_TOTAL: usize = 50;
const GARBLED_BAD_PERCENT: usize = 20;

pub mod ocr;
pub mod render;

pub fn convert_pdf(path: &Path, opts: &ConvertOptions) -> Result<String> {
    let mut t = StageTimer::new();
    // 文字型：pdf-inspector 提取 + 自建阅读顺序；非文字型/失败回退 OCR。
    // --pdf-force-ocr 强制把文字型当图片渲染后 OCR（图片型校准）。
    if !opts.pdf_force_ocr {
        if let Some(md) = text_layer_markdown(path, opts)? {
            return Ok(md);
        }
    }
    // 图片型：PDFium 渲染 + oar-ocr OCR
    // DPI 默认 100（可由 --dpi 调整）。DPI 200→100：像素量降 75%，实测 上海公报52p
    // 148.5s→100.0s(-33%)，内容恢复率零损失(99.83%)；80 起脚注/小字开始漏检。
    let images = render::render_pdf_pages(path, opts.dpi)?;
    t.stage("render");
    let pages = ocr::ocr_images(images, opts.ocr_tier, opts.ocr_layout, opts.threads)?;
    t.stage("ocr");
    let md = gfm_adapter::structure_results_to_gfm(&pages);
    t.stage("gfm");
    Ok(md)
}

/// 文字层 Markdown：pdf-inspector 提取 TextItem → 列感知拆行 → 排序；
/// 含表格页回退 OCR 输出 `<table>` HTML（见模块文档 T2-B）。
///
/// 返回 `None` 表示无可用文字层（扫描件/提取失败），调用方回退 OCR。
fn text_layer_markdown(path: &Path, opts: &ConvertOptions) -> Result<Option<String>> {
    // 坏字体（GID/编码损坏）防护：调用 pdf-inspector 的健壮检测器做一次全文档
    // markdown 抽取（其内部本就全页抽取），统计被判 `suspected_garbled_text` 的页数。
    // 系统性坏字体 → 大量页面乱码（占比高）→ 文字层不可信，回退 OCR；健康文档即使
    // 有少量误报（如目录点线符的私有区字符，上海公报仅 2 页）也不触发。
    // 仅此信号确认命中才回退，出错则忽略继续。拉丁扩展/符号乱码（本地 looks_garbled
    // 检不出）由此兜住。开销约 0.3s（全页 markdown 构建），可接受。
    // 注：原设想"第 1 页 pages_needing_ocr 非空即坏字体"对本样例不成立——封面页
    // 干净而正文全坏，故改为全文档占比判定。
    // 同一次抽取顺带拿到每页 OCR 原因（用于乱码占比判定）。
    match pdf_inspector::extract_pages_markdown(path, None) {
        Ok(extraction) => {
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
            if garbled >= 3 && total > 0 && garbled * 100 >= total * 20 {
                return Ok(None);
            }
        }
        Err(_) => {}
    }
    let items = match pdf_inspector::extract_text_with_positions(path) {
        Ok(items) => items,
        Err(_) => return Ok(None),
    };
    if items.is_empty() {
        return Ok(None);
    }
    // 廉价坏字体防护：提取文本若大量出现替换符/私有区/控制符（GID 坏字体常见
    // 特征），文字层输出是乱码，回退 OCR。正常 PDF 几乎无此类字符，零开销。
    // 注：拉丁扩展乱码（如某些 GID 字体）此处检不出，行为与旧 anydoc 一致。
    if looks_garbled(&items) {
        return Ok(None);
    }

    // 跨页重复"页面家具/水印"剔除：同文本 + 同归一化位置（x 中心、y 各 1% 箱）
    // 出现在 >=pages_needed 个不同页 → 页眉/页脚/水印，剔除后再做行分组。
    // 单页/页数不足时 pages_needed > total_pages → 零剔除（零误杀）。
    let total_pages = items.iter().map(|i| i.page).max().unwrap_or(0) as usize;
    let pages_needed = std::cmp::max(3usize, ((total_pages as f32) * 0.6).ceil() as usize);
    let furniture = is_repeated_furniture(&items, pages_needed, total_pages);
    let items: Vec<pdf_inspector::TextItem> = items
        .into_iter()
        .filter(|i| !furniture.contains(&(i.page, i.x.to_bits(), i.y.to_bits(), i.text.clone())))
        .collect();

    // 按页分组（TextItem.page 1 起始），页序升序
    let mut by_page: BTreeMap<u32, Vec<pdf_inspector::TextItem>> = BTreeMap::new();
    for item in items {
        by_page.entry(item.page).or_default().push(item);
    }

    // 预构建每页行组（列检测与表格启发式共用一次），并缓存每页近似宽/高。
    let mut lines_by_page: BTreeMap<u32, Vec<pdf_inspector::extractor::TextLine>> = BTreeMap::new();
    let mut page_w: BTreeMap<u32, f32> = BTreeMap::new();
    let mut page_h: BTreeMap<u32, f32> = BTreeMap::new();
    for (&page, page_items) in &by_page {
        let mut w = 0.0_f32;
        let mut h = 0.0_f32;
        for i in page_items {
            w = w.max(i.x + i.width);
            h = h.max(i.y + i.height);
        }
        page_w.insert(page, w);
        page_h.insert(page, h);
        lines_by_page.insert(
            page,
            pdf_inspector::extractor::group_into_lines_preserving_all_text(page_items.clone()),
        );
    }

    // ── T2-B/R1/R3：可疑表格页集合（三信号并集，最终确认靠版面 OCR）──
    //  信号1：文字层启发式——某页 >=3 行各自被宽间隙拆成 >=3 个 x 分离段。保守：
    //         双列正文每行仅 2 段（1 条 gutter），不会误报；真表格/目录行多为多列。
    //  信号2：R3 末页探针——末页（最大页号）强制入集（布局模型易漏小表格，如
    //         末页版权栏），并另跑 pdf-inspector 表格提取作结构兜底。
    //  信号3：首页强制入集——公报/刊物封面常有标题块/框线，布局模型可识别为表；
    //         pdf-inspector `pages_with_tables` 易漏首页，故不依赖之。
    // 最终该页是否真出 `<table>`：版面 OCR 检出 `LayoutElementType::Table`
    // 才确认；未确认页回落文字层，防误报（R2 gfm 过滤仍生效）。
    let mut suspicious: BTreeSet<u32> = BTreeSet::new();
    for (&page, lines) in &lines_by_page {
        if page_has_tabular_rows(lines, page_w[&page]) {
            suspicious.insert(page);
        }
    }
    let first_page = *by_page.keys().next().unwrap();
    let last_page = *by_page.keys().next_back().unwrap();
    suspicious.insert(first_page);
    suspicious.insert(last_page);
    // pdf-inspector 末页表格提取探针：命中（非空管道表）→ 布局未确认时兜底输出。
    let last_table_md = probe_last_page_table(path, last_page, &page_w, &page_h);

    // 懒渲染（仅可疑集非空才做）+ 批量版面 OCR（用 `opts.ocr_layout`，默认 Doc：
    // 含 table 类，能识别封面/版权栏等；Table 版面只标 Table，漏检严重）。确认
    // 有 Table 的页 → 单页 gfm（行 + `<table>`）；未确认页 → 回落文字层。
    // 渲染/OCR 任一环节失败 → 该页回落，不炸文档。
    let mut table_out: BTreeMap<u32, String> = BTreeMap::new();
    if !suspicious.is_empty() {
        if let Ok(images) = render::render_pdf_pages(path, opts.dpi) {
            // 渲染输出按文档页序（0 起始）；页号升序，防缺页错位。
            let mut imgs: Vec<image::RgbImage> = Vec::new();
            let mut ocr_pages: Vec<u32> = Vec::new();
            for &p in &suspicious {
                let idx = p as usize - 1;
                if by_page.contains_key(&p) && idx < images.len() {
                    imgs.push(images[idx].clone());
                    ocr_pages.push(p);
                }
            }
            if !imgs.is_empty()
                && let Ok(results) =
                    ocr::ocr_images(imgs, opts.ocr_tier, opts.ocr_layout, opts.threads)
            {
                for (page, res) in ocr_pages.into_iter().zip(results) {
                    let has_table = res.layout_elements.iter().any(|e| {
                        e.element_type
                            == oar_ocr::domain::structure::LayoutElementType::Table
                    });
                    if has_table {
                        table_out.insert(
                            page,
                            gfm_adapter::structure_results_to_gfm(std::slice::from_ref(&res)),
                        );
                    }
                }
            }
        }
    }

    // ── 输出装配：文字层网格表（免 OCR、跨页合并）+ OCR 确认表 + 普通行，页序混排 ──
    let mut segments: BTreeMap<u32, String> = BTreeMap::new();
    let mut pending: Option<(TableGrid, u32)> = None;
    for (page, page_items) in by_page.iter() {
        let page_w = page_w[page];
        let full_lines = &lines_by_page[page];

        // 1) 文字层网格表格（快速、免 OCR）：按页续接合并（B4）
        if let Some(grid) = reconstruct_table_grid(page_items, page_w) {
            match pending.take() {
                Some((mut p, sp)) if p.cols == grid.cols => {
                    extend_table_grid(&mut p, grid);
                    pending = Some((p, sp));
                }
                Some((p, sp)) => {
                    flush_table(&mut segments, p, sp);
                    pending = Some((grid, *page));
                }
                None => pending = Some((grid, *page)),
            }
            continue;
        }

        // 2) 版面 OCR 确认的表格页：直接输出 OCR 通路结果（行 + <table>）
        if let Some(ocr_md) = table_out.get(page) {
            if let Some((p, sp)) = pending.take() {
                flush_table(&mut segments, p, sp);
            }
            segments.entry(*page).or_default().push_str(ocr_md);
            segments.entry(*page).or_default().push_str("\n\n");
            continue;
        }

        // 3) 普通页：先冲掉挂起的跨页表，再输出文字层行
        if let Some((p, sp)) = pending.take() {
            flush_table(&mut segments, p, sp);
        }
        let mut seg_out = String::new();
        // 列间隙检测：行级候选间隙聚类。封面/标题的字母间距是单行现象、每行
        // split_x 各不相同，聚类不到 >=3 行；双列正文的 gutter 在每行同一 x 处
        // 重复出现，聚成主簇 → 只拆这些行，标题行保持整行。
        let split = clustered_row_split(full_lines, page_w);

        let mut regions: Vec<(f32, f32, f32, f32, String)> = Vec::new();
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
                    if gap > 0.01 * page_w {
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
                    push_line_region(&seg, &line, *page, &mut regions);
                    seg = sorted;
                    push_line_region(&seg, &line, *page, &mut regions);
                    continue;
                }
            }
            seg = sorted;
            push_line_region(&seg, &line, *page, &mut regions);
        }

        for t in reading_order::postprocess_lines(reading_order::order_text_regions(&regions)) {
            seg_out.push_str(&t);
            seg_out.push('\n');
        }

        // R3 兜底：末页布局未确认但 pdf-inspector 探针提取到表格（版权栏等小表格）
        // → 文字层行后追加管道表，保证表格信息不丢（保留正文行，仅追加结构）。
        if *page == last_page {
            if let Some(tbl) = &last_table_md {
                seg_out.push('\n');
                seg_out.push_str(tbl);
                seg_out.push('\n');
            }
        }
        seg_out.push('\n');
        segments.entry(*page).or_default().push_str(&seg_out);
    }
    if let Some((p, sp)) = pending.take() {
        flush_table(&mut segments, p, sp);
    }
    let mut out = String::new();
    for (_, seg) in segments {
        out.push_str(&seg);
    }
    let md = out.trim_end().to_string();
    if md.is_empty() {
        Ok(None)
    } else {
        Ok(Some(md))
    }
}

/// 文字层表格单元格（文本 + 几何，用于合并单元格 span 推断）。
#[derive(Clone)]
struct TableCell {
    text: String,
    x: f32,
    y: f32,
    h: f32,
}

/// 文字层表格网格（行×列）。
struct TableGrid {
    cols: usize,
    header: Vec<TableCell>,
    rows: Vec<Vec<TableCell>>,
}

fn empty_cell() -> TableCell {
    TableCell {
        text: String::new(),
        x: 0.0,
        y: 0.0,
        h: 0.0,
    }
}

/// 从一页原始 TextItem 重建表格网格：按 y 组行、行内按 x 间隙聚列。
/// 要求：列数>=2 且 >=2 行同列数；**列 x 对齐**（同列首格 x 散布小，双列正文参差则拒）。
/// 长句/散文不再拒表（对齐 MinerU：长文本保留在单元格，判表靠列结构）。
/// 不使用 `group_into_lines`（其会把单元格拆成独立行）。
fn reconstruct_table_grid(
    page_items: &[pdf_inspector::TextItem],
    page_w: f32,
) -> Option<TableGrid> {
    if page_items.len() < 4 {
        return None;
    }
    let mut sorted: Vec<&pdf_inspector::TextItem> = page_items.iter().collect();
    sorted.sort_by(|a, b| b.y.total_cmp(&a.y)); // PDF 左下原点：y 大=靠上
    let row_tol = 4.0_f32;
    let gap_thr = 0.012 * page_w;
    let mut rows: Vec<Vec<TableCell>> = Vec::new();
    let mut cur: Vec<&pdf_inspector::TextItem> = Vec::new();
    let mut cur_y = 0.0_f32;
    for it in sorted {
        if !cur.is_empty() && (cur_y - it.y).abs() > row_tol {
            rows.push(cluster_row(&cur, gap_thr));
            cur.clear();
        }
        if cur.is_empty() {
            cur_y = it.y;
        }
        cur.push(it);
    }
    if !cur.is_empty() {
        rows.push(cluster_row(&cur, gap_thr));
    }
    // 只保留列数 >=2 的行（丢弃单格散落文本）
    let rows: Vec<Vec<TableCell>> = rows.into_iter().filter(|r| r.len() >= 2).collect();
    if rows.len() < 2 {
        return None;
    }
    let mut counts: std::collections::BTreeMap<usize, usize> = std::collections::BTreeMap::new();
    for r in &rows {
        *counts.entry(r.len()).or_default() += 1;
    }
    let (cols, col_rows) = counts.iter().max_by_key(|(_, c)| **c)?;
    let (cols, col_rows) = (*cols, *col_rows);
    if cols < 2 || col_rows < 2 {
        return None;
    }
    // 对齐到众数列数（不足补空）
    let mut aligned: Vec<Vec<TableCell>> = Vec::new();
    for r in &rows {
        let mut a = r.clone();
        a.resize(cols, empty_cell());
        aligned.push(a);
    }
    // 列 x 对齐检测：同列首格 x 散布 > 容差 → 列参差（双列正文）→ 拒
    let col_tol = (0.02 * page_w).max(10.0);
    for c in 0..cols {
        let xs: Vec<f32> = aligned
            .iter()
            .filter(|r| !r[c].text.is_empty())
            .map(|r| r[c].x)
            .collect();
        if xs.len() >= 2 {
            let mn = xs.iter().copied().fold(f32::INFINITY, f32::min);
            let mx = xs.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            if mx - mn > col_tol {
                return None;
            }
        }
    }
    let header = aligned[0].clone();
    let rows = aligned[1..].to_vec();
    Some(TableGrid { cols, header, rows })
}

fn cell_texts(v: &[TableCell]) -> Vec<String> {
    v.iter().map(|c| c.text.clone()).collect()
}

/// 跨页续接：列数一致时合并（若下页首行 == 已有表头 → 去重表头）。
fn extend_table_grid(acc: &mut TableGrid, next: TableGrid) {
    if next.cols != acc.cols {
        return;
    }
    let first_matches = next
        .rows
        .first()
        .map(|r| cell_texts(r) == cell_texts(&acc.header))
        .unwrap_or(false);
    if first_matches {
        acc.rows.extend(next.rows.iter().skip(1).cloned());
    } else {
        acc.rows.extend(next.rows);
    }
}

/// 把已定型的跨页表写入其"首表页"段（保证阅读顺序）。
fn flush_table(segments: &mut BTreeMap<u32, String>, grid: TableGrid, start_page: u32) {
    let e = segments.entry(start_page).or_default();
    e.push_str(&table_grid_to_html(&grid));
    e.push('\n');
    e.push('\n');
}

/// 行内按 x 间隙聚列，返回 (首格 x, y, 高, 文本) 的单元格。
fn cluster_row(items: &[&pdf_inspector::TextItem], gap_thr: f32) -> Vec<TableCell> {
    let mut cells: Vec<TableCell> = Vec::new();
    let mut cluster: Vec<pdf_inspector::TextItem> = Vec::new();
    let mut x0 = 0.0_f32;
    let mut y0 = 0.0_f32;
    let mut hmax = 0.0_f32;
    for &it in items {
        if let Some(prev) = cluster.last() {
            if it.x - (prev.x + prev.width) > gap_thr {
                cells.push(TableCell {
                    text: join_cell_items(&cluster),
                    x: x0,
                    y: y0,
                    h: hmax,
                });
                cluster.clear();
            }
        }
        if cluster.is_empty() {
            x0 = it.x;
            y0 = it.y;
            hmax = 0.0;
        }
        cluster.push(it.clone());
        hmax = hmax.max(it.height);
    }
    if !cluster.is_empty() {
        cells.push(TableCell {
            text: join_cell_items(&cluster),
            x: x0,
            y: y0,
            h: hmax,
        });
    }
    cells
}

/// 单元格内多个 TextItem 拼接：仅 ASCII 字母数字间加空格（CJK 不加）。
fn join_cell_items(items: &[pdf_inspector::TextItem]) -> String {
    let mut s = String::new();
    for (i, it) in items.iter().enumerate() {
        if i > 0 {
            let a = s
                .chars()
                .last()
                .map(|c| c.is_ascii_alphanumeric())
                .unwrap_or(false);
            let b = it
                .text
                .chars()
                .next()
                .map(|c| c.is_ascii_alphanumeric())
                .unwrap_or(false);
            if a && b {
                s.push(' ');
            }
        }
        s.push_str(it.text.trim());
    }
    s
}

/// TableGrid → `<table><thead>…</thead><tbody>…</tbody></table>`。
/// 合并单元格：行内尾空 → colspan；高 cell（h > 1.5×行距）→ rowspan 并吞下方空位。
fn table_grid_to_html(g: &TableGrid) -> String {
    // 行距估计 = 相邻行首格 y 差的中位数（跨页边界跳变会拉大中位，抑制误判 rowspan）
    let mut ys: Vec<f32> = Vec::new();
    for row in std::iter::once(&g.header).chain(g.rows.iter()) {
        if let Some(c) = row.iter().find(|c| !c.text.is_empty()) {
            ys.push(c.y);
        }
    }
    let mut pitch = 12.0_f32;
    if ys.len() >= 2 {
        let mut gaps: Vec<f32> = ys.windows(2).map(|w| (w[0] - w[1]).abs()).collect();
        gaps.sort_by(|a, b| a.total_cmp(b));
        pitch = gaps[gaps.len() / 2].max(4.0);
    }
    let mut s = String::from("<table><thead><tr>");
    let mut c = 0;
    while c < g.header.len() {
        let cell = &g.header[c];
        let mut colspan = 1;
        while c + colspan < g.header.len() && g.header[c + colspan].text.is_empty() {
            colspan += 1;
        }
        let attr = span_attr(colspan, 1);
        s.push_str(&format!("<td{attr}>{}</td>", escape_html(&cell.text)));
        c += colspan;
    }
    s.push_str("</tr></thead><tbody>");
    let nrows = g.rows.len();
    let mut skip = vec![vec![false; g.cols]; nrows];
    for ri in 0..nrows {
        s.push_str("<tr>");
        let mut c = 0;
        while c < g.cols {
            if skip[ri][c] {
                c += 1;
                continue;
            }
            let cell = &g.rows[ri][c];
            let mut colspan = 1;
            while c + colspan < g.cols
                && g.rows[ri][c + colspan].text.is_empty()
                && !skip[ri][c + colspan]
            {
                colspan += 1;
            }
            let mut rowspan = 1;
            if !cell.text.is_empty() && cell.h > pitch * 1.5 && pitch > 0.0 {
                let est = (cell.h / pitch).round().max(1.0) as usize;
                rowspan = est.min(nrows - ri).max(1);
                if rowspan > 1 {
                    for k in (ri + 1)..(ri + rowspan).min(nrows) {
                        if c < g.cols && g.rows[k][c].text.is_empty() {
                            skip[k][c] = true;
                        }
                    }
                }
            }
            let attr = span_attr(colspan, rowspan);
            s.push_str(&format!("<td{attr}>{}</td>", escape_html(&cell.text)));
            c += colspan;
        }
        s.push_str("</tr>");
    }
    s.push_str("</tbody></table>");
    s
}

fn span_attr(colspan: usize, rowspan: usize) -> String {
    let mut a = String::new();
    if colspan > 1 {
        a.push_str(&format!(" colspan=\"{colspan}\""));
    }
    if rowspan > 1 {
        a.push_str(&format!(" rowspan=\"{rowspan}\""));
    }
    a
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// 从每行内找出"列间隙"候选（gap 中点），按 x 聚类；主簇 >=3 行才返回全局 split_x。
///
/// 双列页：每行的 gutter 都在同一 x → 聚成主簇。封面大标题字母间距大但每行
/// split_x 不同/行数少 → 主簇不足 3 → 返回 None，行保持整行。
fn clustered_row_split(
    lines: &[pdf_inspector::extractor::TextLine],
    page_w: f32,
) -> Option<f32> {
    let min_gap = 0.01 * page_w;
    let tol = 0.02 * page_w;
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
fn page_has_tabular_rows(
    lines: &[pdf_inspector::extractor::TextLine],
    page_w: f32,
) -> bool {
    let min_gap = 0.01 * page_w;
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
fn probe_last_page_table(
    path: &Path,
    last_page: u32,
    page_w: &BTreeMap<u32, f32>,
    page_h: &BTreeMap<u32, f32>,
) -> Option<String> {
    let buf = std::fs::read(path).ok()?;
    let w = page_w.get(&last_page).copied().unwrap_or(595.0);
    let h = page_h.get(&last_page).copied().unwrap_or(842.0);
    let regions = [(last_page - 1, vec![[0.0, 0.0, w + 40.0, h + 40.0]])];
    let results = pdf_inspector::extract_tables_in_regions_mem(&buf, &regions).ok()?;
    let md = results
        .into_iter()
        .next()?
        .regions
        .into_iter()
        .next()?
        .text;
    let md = md.trim();
    (!md.is_empty()).then(|| md.to_string())
}

/// 坏字体乱码检测：前 4000 个 TextItem 中替换符 `\u{FFFD}`、私有区
/// (U+E000..=U+F8FF)、控制字符占比达 20% 且字符总数 >50 → 判定乱码，
/// 文字层应回退 OCR。
fn looks_garbled(items: &[pdf_inspector::TextItem]) -> bool {
    let mut total = 0usize;
    let mut bad = 0usize;
    for item in items.iter().take(GARBLED_MAX_ITEMS) {
        for c in item.text.chars() {
            total += 1;
            if c == '\u{FFFD}'
                || ('\u{E000}'..='\u{F8FF}').contains(&c)
                || c.is_control()
            {
                bad += 1;
            }
        }
    }
    total > GARBLED_MIN_TOTAL && bad * 100 >= total * GARBLED_BAD_PERCENT
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
    regions: &mut Vec<(f32, f32, f32, f32, String)>,
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
    regions.push((x_min, x_max, y_flip, y_flip + (y_max_pdf - line.y).max(1.0), text));
}

#[cfg(test)]
mod tests {
    use super::{
        cell_texts, clustered_row_split, extend_table_grid, is_repeated_furniture,
        looks_garbled, reconstruct_table_grid, table_grid_to_html, TableGrid,
    };
    use pdf_inspector::extractor::TextLine;
    use pdf_inspector::TextItem;

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
        let items = vec![ti(&format!("{}{}", "a".repeat(30), "\u{FFFD}".repeat(30)), 0.0, 10.0)];
        assert!(looks_garbled(&items));
    }

    /// 正常 CJK/Latin 文本 → 非乱码。
    #[test]
    fn normal_text_is_not_garbled() {
        let items = vec![ti("你好，世界 Hello World, this is a normal sentence.", 0.0, 10.0)];
        assert!(!looks_garbled(&items));
    }

    /// 同文本 + 同归一化位置出现在 5/6 页（pages_needed=4）→ 判为家具，签名剔除。
    /// 正文每页不同（真实正文如此），不受影响。
    #[test]
    fn same_text_same_position_on_most_pages_is_furniture() {
        let mut items: Vec<TextItem> = Vec::new();
        for page in 1..=6u32 {
            // 正文每页不同，保证各页 page_max 一致
            items.push(tif(&format!("正文内容第{page}页"), 100.0, 400.0, 200.0, 10.0, page));
            if page <= 5 {
                // 页眉：页 1..5 同一位置
                items.push(tif("上海市人民政府公报 2025·1", 200.0, 800.0, 100.0, 10.0, page));
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
            items.push(tif(&format!("正文内容第{page}页"), 100.0, 400.0, 200.0, 10.0, page));
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
            items.push(tif(&format!("正文内容第{page}页"), 100.0, 400.0, 200.0, 10.0, page));
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

    fn tcell(t: &str, x: f32, y: f32) -> TextItem {
        tif(t, x, y, 20.0, 10.0, 1)
    }

    fn tc(t: &str, x: f32, y: f32, h: f32) -> super::TableCell {
        super::TableCell {
            text: t.into(),
            x,
            y,
            h,
        }
    }

    #[test]
    fn grid_table_reconstructed_from_items() {
        // 3 行 × 2 列：header + 2 数据行
        let items = vec![
            tcell("ID", 10.0, 90.0),
            tcell("Name", 60.0, 90.0),
            tcell("1", 10.0, 80.0),
            tcell("Alice", 60.0, 80.0),
            tcell("2", 10.0, 70.0),
            tcell("Bob", 60.0, 70.0),
        ];
        let g = reconstruct_table_grid(&items, 200.0).expect("grid");
        assert_eq!(g.cols, 2);
        assert_eq!(cell_texts(&g.header), vec!["ID", "Name"]);
        assert_eq!(g.rows.len(), 2);
        assert_eq!(cell_texts(&g.rows[1]), vec!["2", "Bob"]);
    }

    #[test]
    fn aligned_two_column_prose_kept_as_table() {
        // 列 x 对齐 + 长句：按 MinerU 保留（长文本在单元格，不因长句拒表）
        let items = vec![
            tcell("备注（本栏为长文本说明示例，用于验证长句不被拒表）", 10.0, 90.0),
            tcell("值1", 160.0, 90.0),
            tcell("第二条 本条规定了处罚的适用情形，应当严格遵照执行。", 10.0, 80.0),
            tcell("值2", 160.0, 80.0),
            tcell("第三条 管理部门应当依法履行职责并接受社会监督。", 10.0, 70.0),
            tcell("值3", 160.0, 70.0),
        ];
        assert!(reconstruct_table_grid(&items, 300.0).is_some());
    }

    #[test]
    fn ragged_two_column_body_not_table() {
        // 双列正文：同列首格 x 参差（段落缩进/对齐不规则）→ 拒
        let items = vec![
            tcell("经研究，市人民政府决定，对下列市政府规章予以修改和废止。", 10.0, 90.0),
            tcell("修改为：", 150.0, 90.0),
            tcell("一、对下列政府规章的部分条款予以修改，现予公布施行。", 40.0, 80.0),
            tcell("受市生态环境部门委托，负责放射源销售单位的许可证核发。", 170.0, 80.0),
            tcell("（一）上海市放射性污染防治若干规定，自2025年1月1日起施行。", 25.0, 70.0),
            tcell("前增加“Ⅱ类”，并采取有效措施防止放射性污染。", 185.0, 70.0),
        ];
        assert!(reconstruct_table_grid(&items, 300.0).is_none());
    }

    #[test]
    fn single_column_not_table() {
        let items = vec![
            tcell("A", 10.0, 90.0),
            tcell("B", 10.0, 80.0),
            tcell("C", 10.0, 70.0),
        ];
        assert!(reconstruct_table_grid(&items, 200.0).is_none());
    }

    #[test]
    fn cross_page_merge_drops_repeated_header() {
        let mut acc = TableGrid {
            cols: 2,
            header: vec![tc("ID", 10.0, 0.0, 10.0), tc("Name", 60.0, 0.0, 10.0)],
            rows: vec![vec![tc("1", 10.0, 0.0, 10.0), tc("Alice", 60.0, 0.0, 10.0)]],
        };
        // 下页：重复表头 + 续行
        let next = TableGrid {
            cols: 2,
            header: vec![tc("ID", 10.0, 0.0, 10.0), tc("Name", 60.0, 0.0, 10.0)],
            rows: vec![
                vec![tc("ID", 10.0, 0.0, 10.0), tc("Name", 60.0, 0.0, 10.0)],
                vec![tc("2", 10.0, 0.0, 10.0), tc("Bob", 60.0, 0.0, 10.0)],
            ],
        };
        extend_table_grid(&mut acc, next);
        assert_eq!(acc.rows.len(), 2, "表头去重，仅剩 Alice/Bob");
        assert_eq!(cell_texts(&acc.rows[1]), vec!["2", "Bob"]);
    }

    #[test]
    fn html_emits_span_attributes() {
        // colspan：行内尾空（备注跨 2 列）；rowspan：高 cell（备注跨 2 行）
        let g = TableGrid {
            cols: 3,
            header: vec![tc("A", 10.0, 100.0, 10.0), tc("B", 60.0, 100.0, 10.0), tc("C", 110.0, 100.0, 10.0)],
            rows: vec![
                vec![tc("1", 10.0, 90.0, 10.0), tc("x", 60.0, 90.0, 10.0), tc("", 0.0, 0.0, 0.0)],
                vec![tc("2", 10.0, 80.0, 10.0), tc("y", 60.0, 80.0, 10.0), tc("z", 110.0, 80.0, 10.0)],
            ],
        };
        let html = table_grid_to_html(&g);
        // 行 0 的 "x" 因 c=1 后尾空 → colspan=2
        assert!(html.contains("colspan=\"2\""), "期望 colspan，got: {html}");
        assert!(!html.contains("rowspan"), "rowspan 不应出现: {html}");
    }
}
