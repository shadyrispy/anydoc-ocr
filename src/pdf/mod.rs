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
//! T2-A：文字层输出为纯行文本（与 OCR 通路的正文行一致），表格**不会**以 HTML
//! 形式输出（已知限制）；OCR 通路 / `--pdf-force-ocr` 会输出表格 HTML。
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
        if let Some(md) = text_layer_markdown(path)? {
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

/// 文字层 Markdown：pdf-inspector 提取 TextItem → 列感知拆行 → 排序。
///
/// 返回 `None` 表示无可用文字层（扫描件/提取失败），调用方回退 OCR。
fn text_layer_markdown(path: &Path) -> Result<Option<String>> {
    // 坏字体（GID/编码损坏）防护：调用 pdf-inspector 的健壮检测器做一次全文档
    // markdown 抽取（其内部本就全页抽取），统计被判 `suspected_garbled_text` 的页数。
    // 系统性坏字体 → 大量页面乱码（占比高）→ 文字层不可信，回退 OCR；健康文档即使
    // 有少量误报（如目录点线符的私有区字符，上海公报仅 2 页）也不触发。
    // 仅此信号确认命中才回退，出错则忽略继续。拉丁扩展/符号乱码（本地 looks_garbled
    // 检不出）由此兜住。开销约 0.3s（全页 markdown 构建），可接受。
    // 注：原设想"第 1 页 pages_needing_ocr 非空即坏字体"对本样例不成立——封面页
    // 干净而正文全坏，故改为全文档占比判定。
    if let Ok(extraction) = pdf_inspector::extract_pages_markdown(path, None) {
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

    let mut out = String::new();
    for (page, page_items) in by_page {
        let page_w = page_items
            .iter()
            .map(|i| i.x + i.width)
            .fold(0.0_f32, f32::max);
        let full_lines =
            pdf_inspector::extractor::group_into_lines_preserving_all_text(page_items);

        // 列间隙检测：行级候选间隙聚类。封面/标题的字母间距是单行现象、每行
        // split_x 各不相同，聚类不到 >=3 行；双列正文的 gutter 在每行同一 x 处
        // 重复出现，聚成主簇 → 只拆这些行，标题行保持整行。
        let split = clustered_row_split(&full_lines, page_w);

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
                    push_line_region(&seg, &line, page, &mut regions);
                    seg = sorted;
                    push_line_region(&seg, &line, page, &mut regions);
                    continue;
                }
            }
            seg = sorted;
            push_line_region(&seg, &line, page, &mut regions);
        }

        for t in reading_order::postprocess_lines(reading_order::order_text_regions(&regions)) {
            out.push_str(&t);
            out.push('\n');
        }
        out.push('\n');
    }

    let md = out.trim_end().to_string();
    if md.is_empty() {
        Ok(None)
    } else {
        Ok(Some(md))
    }
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
    use super::{clustered_row_split, is_repeated_furniture, looks_garbled};
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
}
