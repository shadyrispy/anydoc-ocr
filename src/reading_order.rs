//! 文本区域的阅读顺序还原（双列/多列感知排序）。
//!
//! OCR 通路（`gfm_adapter`）与文字层通路（`pdf/mod.rs`）共用同一排序算法：
//! 把所有区域按 x 中心排序，取**最大间隙**切分出列——双列页面的列间 gutter
//! 正是最大间隙；单栏页面的最大间隙很小（只是列内相邻行的 x 抖动），不触发
//! 切分。每列内按 y 排序、列间从左到右。
//!
//! 关键点：列检测用**所有**文本区域（含宽条目）。早先版本把跨度>60% 页宽的
//! 元素当作"整宽"剔除，但公报目录里左列的长标题（如"上海市人民政府办公厅
//! 关于转发市交通委制订的《上海港口基础设施维护管理办法》的通知"）跨度就达
//! ~68% 页宽，被误删后列间隙被糊掉、分栏失败。因此只把**真正跨整页**（x 同时
//! 贴近左右边距）的元素（页眉/页脚/通栏标题）剔除为 header/footer，左列长条目
//! 按其 x 中心自然归入左列。
//!
//! 无清晰间隙（单栏或无法切分）时退化为纯 y 排序，兼容单栏文档与边缘情况。
//!
//! `regions`: [`Region`]（`x_min/x_max/y_min/y_max/文本`）。

use crate::region::Region;
use oar_ocr::domain::structure::{LayoutElement, LayoutElementType, RegionBlock, StructureResult};

/// 噪声块类型集合（ADR-0009 D4：按类型过滤，对齐 MinerU BlockType 丢弃）。
/// 首版不设置信度阈值（对齐 MinerU 源码：按类型全量分派）。
const NOISE_TYPES: &[LayoutElementType] = &[
    LayoutElementType::Header,
    LayoutElementType::HeaderImage,
    LayoutElementType::Footer,
    LayoutElementType::FooterImage,
    LayoutElementType::Number,
    LayoutElementType::Seal,
];

/// OCR 文本区域的阅读顺序还原。
///
/// `y` 语义：**越小越靠上**（图像坐标系，原点左上）。PDF 坐标（原点左下）
/// 需在调用方翻转后传入，否则上下颠倒。
pub fn order_text_regions(regions: &[Region]) -> Vec<String> {
    if regions.is_empty() {
        return Vec::new();
    }
    let page_w = Region::page_w(regions);
    if page_w <= 0.0 {
        return sort_by_y(regions);
    }
    let Some(split) = detect_column_split(regions) else {
        return sort_by_y(regions);
    };

    // 列分类：左/右/整宽三组（复用 split_columns，消除与 order_within_block 的重复）
    let (left_refs, right_refs, full_refs) = split_columns(regions, split);
    let mut left: Vec<(f32, String)> = left_refs
        .iter()
        .map(|r| (r.y_min, r.text.clone()))
        .collect();
    let mut right: Vec<(f32, String)> = right_refs
        .iter()
        .map(|r| (r.y_min, r.text.clone()))
        .collect();
    left.sort_by(ord_y);
    right.sort_by(ord_y);

    // 整宽元素按 y 归页眉(y<正文起点)/页脚(y>正文终点)/正文区间(罕见置后)
    let full: Vec<(f32, String)> = full_refs
        .iter()
        .map(|r| (r.y_min, r.text.clone()))
        .collect();
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
    let mut head: Vec<_> = full
        .iter()
        .filter(|(y, _)| *y < body_min)
        .cloned()
        .collect();
    let mut foot: Vec<_> = full
        .iter()
        .filter(|(y, _)| *y > body_max)
        .cloned()
        .collect();
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
    out
}

/// 检测双列切分线（gutter）。返回 `None` 表示单栏/无法切分。
///
/// 列检测用**所有**正文区域（含宽条目）的中心 x，取最大间隙切分；要求间隙
/// >= 3% 页宽且两侧各 >=2 区域，避免把单栏内的大间距误判为分栏。真正跨整页
/// > （x 同时贴近左右边距）的元素（页眉/页脚/通栏标题）先剔除。
pub fn detect_column_split(regions: &[Region]) -> Option<f32> {
    if regions.len() < 4 {
        return None;
    }
    let page_w = Region::page_w(regions);
    if page_w <= 0.0 {
        return None;
    }
    let body_regions: Vec<&Region> = regions
        .iter()
        .filter(|r| !r.is_full_width(page_w))
        .collect();
    let mut body: Vec<f32> = body_regions.iter().map(|r| r.center_x()).collect();
    if body.len() < 4 {
        return None;
    }
    body.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut best_gap = 0.0_f32;
    let mut best_i = 1usize;
    for i in 1..body.len() {
        let g = body[i] - body[i - 1];
        if g > best_gap {
            best_gap = g;
            best_i = i;
        }
    }
    let min_gap = 0.03 * page_w;
    if best_gap < min_gap || best_i < 2 || (body.len() - best_i) < 2 {
        return None;
    }
    let split = (body[best_i - 1] + body[best_i]) / 2.0;
    // 傀栏 vs 单栏判别：真实列间隙是一段无文本的竖直空白带——页内没有任何区域
    // 跨过该中线（左列区域右缘 < split、右列区域左缘 > split）。单栏页行宽天然
    // 变化（短标签 + 通栏段落），最大 center_x 间隙往往是相邻两行的宽度差，
    // 全宽正文区域会跨过该"假间隙"。若存在跨过分隔线的区域 → 非真列 → 单栏。
    // 修复：9001c 文字版 4.1/4.2 节正文大量缺行（误判双列后正文被颠倒/切割）。
    let bridges = body_regions
        .iter()
        .any(|r| r.x_min < split && split < r.x_max);
    if bridges {
        return None;
    }
    Some(split)
}

fn ord_y(a: &(f32, String), b: &(f32, String)) -> std::cmp::Ordering {
    a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal)
}

/// 单栏/无可切分列时的退化为纯 y 排序（保持旧行为兼容）
fn sort_by_y(regions: &[Region]) -> Vec<String> {
    let mut v: Vec<(f32, String)> = regions.iter().map(|r| (r.y_min, r.text.clone())).collect();
    v.sort_by(ord_y);
    v.into_iter().map(|(_, t)| t).collect()
}

/// 将 region 按列切分线 `split` 分为左/右/整宽三组（消除 `order_text_regions` 与
/// `order_within_block` 的列分类重复）。
fn split_columns<'a>(
    regions: &'a [Region],
    split: f32,
) -> (Vec<&'a Region>, Vec<&'a Region>, Vec<&'a Region>) {
    let page_w = Region::page_w(regions);
    let mut left = Vec::new();
    let mut right = Vec::new();
    let mut full = Vec::new();
    for r in regions {
        if r.is_full_width(page_w) {
            full.push(r);
        } else if r.center_x() < split {
            left.push(r);
        } else {
            right.push(r);
        }
    }
    (left, right, full)
}

/// 页内尺度归一化：text_regions（原图尺度）与 layout_elements bbox（模型 resize 尺度）
/// 坐标系不同。返回 `(t_max_x, t_max_y, l_max_x, l_max_y)`，供归一化比较使用。
pub(crate) fn page_scale(page: &StructureResult) -> (f32, f32, f32, f32) {
    let mut tw = 0.0_f32;
    let mut th = 0.0_f32;
    if let Some(regs) = &page.text_regions {
        for r in regs {
            tw = tw.max(r.bounding_box.x_max());
            th = th.max(r.bounding_box.y_max());
        }
    }
    let mut lw = 0.0_f32;
    let mut lh = 0.0_f32;
    for el in &page.layout_elements {
        lw = lw.max(el.bbox.x_max());
        lh = lh.max(el.bbox.y_max());
    }
    (tw, th, lw, lh)
}

/// 归一化判定：text 尺度点 `(cx, cy)` 是否落在 layout 尺度 bbox `lb` 内。
/// 各自除以页内最大值转 [0,1]，消除 text/layout 两套坐标尺度差。
pub(crate) fn norm_membership(
    cx: f32,
    cy: f32,
    (tw, th, lw, lh): (f32, f32, f32, f32),
    lb: &oar_ocr::processors::BoundingBox,
) -> bool {
    if tw <= 0.0 || th <= 0.0 || lw <= 0.0 || lh <= 0.0 {
        return false;
    }
    let tx = cx / tw;
    let ty = cy / th;
    let ix0 = lb.x_min() / lw;
    let ix1 = lb.x_max() / lw;
    let iy0 = lb.y_min() / lh;
    let iy1 = lb.y_max() / lh;
    tx >= ix0 && tx <= ix1 && ty >= iy0 && ty <= iy1
}

/// ADR-0011：块驱动阅读序。三级降级链：
/// 1. `LayoutElement::order_index` 排序（模型阅读序，对齐 MinerU `index`）
/// 2. `RegionBlock::order_index` + `element_indices`（PP-DocBlockLayout 列级分组）
/// 3. 现有 `order_text_regions`（区域驱动兜底，行为等价现状）
///
/// 噪声类型块（Header/Footer/Number/Seal）直接跳过（D4）。
/// 块内：收集中心点落在 bbox 内的 regions → 块内列检测（Q6）→ 段落合并（D3）。
/// 返回段落列表（已是合并后的行）。
///
/// 这是 OCR 通路入口，与 `order_text_regions`（文字层通路入口）同级。
pub fn order_structure(page: &StructureResult, regions: &[Region]) -> Vec<String> {
    if regions.is_empty() {
        return Vec::new();
    }
    // 收集有效块（跳过噪声类型），按 order_index 排序；None 的块排末尾按 bbox.y_min
    let mut blocks: Vec<&LayoutElement> = page
        .layout_elements
        .iter()
        .filter(|el| !NOISE_TYPES.contains(&el.element_type))
        .collect();
    let has_order = blocks.iter().any(|el| el.order_index.is_some());
    if !has_order {
        return fallback_order(page, regions);
    }
    blocks.sort_by(|a, b| match (a.order_index, b.order_index) {
        (Some(i), Some(j)) => i.cmp(&j),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a
            .bbox
            .y_min()
            .partial_cmp(&b.bbox.y_min())
            .unwrap_or(std::cmp::Ordering::Equal),
    });

    let scale = page_scale(page);
    let mut consumed: Vec<bool> = vec![false; regions.len()];
    let mut out = assemble_blocks(&blocks, regions, scale, &mut consumed);
    append_leftover(page, regions, scale, &consumed, &mut out);
    out
}

/// 块级装配（消除 block_driven_order / fallback_order / leftover 的循环重复）：
/// 对每个块收集中心点落在 bbox 内的未消费 regions → 块内列检测 + 段落合并。
fn assemble_blocks<'a>(
    blocks: &[&'a LayoutElement],
    regions: &'a [Region],
    scale: (f32, f32, f32, f32),
    consumed: &mut [bool],
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for blk in blocks {
        let inner_idx: Vec<usize> = regions
            .iter()
            .enumerate()
            .filter(|(i, r)| {
                !consumed[*i] && norm_membership(r.center_x(), r.y_min, scale, &blk.bbox)
            })
            .map(|(i, _)| i)
            .collect();
        if inner_idx.is_empty() {
            continue;
        }
        for &i in &inner_idx {
            consumed[i] = true;
        }
        let inner: Vec<Region> = inner_idx.iter().map(|&i| regions[i].clone()).collect();
        out.extend(merge_into_paragraphs(&order_within_block(&inner)));
    }
    out
}

/// 未被任何块消费的 regions（bbox 不匹配，模型漏检）：追加到末尾，按 y 排序。
/// 排除落在噪声块内的 region（避免页眉/页码混入 leftover）。
fn append_leftover(
    page: &StructureResult,
    regions: &[Region],
    scale: (f32, f32, f32, f32),
    consumed: &[bool],
    out: &mut Vec<String>,
) {
    let noise_bboxes: Vec<&oar_ocr::processors::BoundingBox> = page
        .layout_elements
        .iter()
        .filter(|el| NOISE_TYPES.contains(&el.element_type))
        .map(|el| &el.bbox)
        .collect();
    let mut leftover: Vec<&Region> = regions
        .iter()
        .enumerate()
        .filter(|(i, r)| {
            !consumed[*i]
                && !noise_bboxes
                    .iter()
                    .any(|nb| norm_membership(r.center_x(), r.y_min, scale, nb))
        })
        .map(|(_, r)| r)
        .collect();
    if !leftover.is_empty() {
        leftover.sort_by(|a, b| {
            a.y_min
                .partial_cmp(&b.y_min)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let lines: Vec<(f32, String)> =
            leftover.iter().map(|r| (r.y_min, r.text.clone())).collect();
        out.extend(merge_into_paragraphs(&lines));
    }
}

/// ADR-0009 D2：三级降级链——order_index 全 None 时调用。
fn fallback_order(page: &StructureResult, regions: &[Region]) -> Vec<String> {
    if let Some(rbs) = &page.region_blocks {
        let mut sorted_rbs: Vec<&RegionBlock> =
            rbs.iter().filter(|rb| rb.order_index.is_some()).collect();
        sorted_rbs.sort_by_key(|rb| rb.order_index.unwrap());
        if !sorted_rbs.is_empty() {
            let scale = page_scale(page);
            let mut out: Vec<String> = Vec::new();
            let mut consumed: Vec<bool> = vec![false; regions.len()];
            for rb in &sorted_rbs {
                let mut els: Vec<&LayoutElement> = rb
                    .element_indices
                    .iter()
                    .filter_map(|&i| page.layout_elements.get(i))
                    .filter(|el| !NOISE_TYPES.contains(&el.element_type))
                    .collect();
                els.sort_by(|a, b| match (a.order_index, b.order_index) {
                    (Some(i), Some(j)) => i.cmp(&j),
                    _ => a
                        .bbox
                        .y_min()
                        .partial_cmp(&b.bbox.y_min())
                        .unwrap_or(std::cmp::Ordering::Equal),
                });
                out.extend(assemble_blocks(&els, regions, scale, &mut consumed));
            }
            if !out.is_empty() {
                return out;
            }
        }
    }
    // 末选：现有区域驱动
    order_text_regions(regions)
}

/// ADR-0009 D3+Q6：块内排序——列检测收窄到块内 + y 排序。
///
/// 复用 `detect_column_split` 的最大间隙逻辑，但作用域从全页收窄到单块。
/// 单块裹双列（模型把双列正文判成 1 个 Text 块）时分离为左列全→右列全；否则 y 排序。
/// 返回 `(y, text)` 元组，供 `merge_into_paragraphs` 按行距合并。
fn order_within_block(regions: &[Region]) -> Vec<(f32, String)> {
    if regions.is_empty() {
        return Vec::new();
    }
    if let Some(split) = detect_column_split(regions) {
        let (left_refs, right_refs, full_refs) = split_columns(regions, split);
        let mut left: Vec<(f32, String)> = left_refs
            .iter()
            .map(|r| (r.y_min, r.text.clone()))
            .collect();
        let mut right: Vec<(f32, String)> = right_refs
            .iter()
            .map(|r| (r.y_min, r.text.clone()))
            .collect();
        let mut mid: Vec<(f32, String)> = full_refs
            .iter()
            .map(|r| (r.y_min, r.text.clone()))
            .collect();
        left.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        right.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        mid.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        return mid.into_iter().chain(left).chain(right).collect();
    }
    // 单列：纯 y 排序
    let mut v: Vec<(f32, String)> = regions.iter().map(|r| (r.y_min, r.text.clone())).collect();
    v.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    v
}

/// ADR-0009 D3：段落合并——相邻行 y 间距 < 行高 1.5x → 同段。
///
/// 对齐 MinerU `_merge_para_text`：行间无空行（间距小）则合并为一段。
/// 行高用块内中位 region 高度估计；空行/标题行不参与合并。
fn merge_into_paragraphs(lines: &[(f32, String)]) -> Vec<String> {
    if lines.is_empty() {
        return Vec::new();
    }
    // 行高估计：中位 region 高度（无 height 字段，用相邻行 y 差近似）
    let mut gaps: Vec<f32> = Vec::new();
    for w in lines.windows(2) {
        gaps.push((w[1].0 - w[0].0).abs());
    }
    gaps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_gap = gaps.get(gaps.len() / 2).copied().unwrap_or(20.0).max(8.0);
    let merge_threshold = median_gap * 1.5;

    let mut out: Vec<String> = Vec::new();
    let mut cur = lines[0].1.clone();
    for w in lines.windows(2) {
        let gap = (w[1].0 - w[0].0).abs();
        // F3：标题检测需同时覆盖两条通路——OCR 通路标题前缀(`#`)在装配后才加，
        // 合并时只有编号启发式(`title_level`)可用；文字层通路行已带 `#` 前缀。
        // 两者取并集，否则任一路径的标题行都可能被并入正文段。
        let is_heading = |s: &str| title_level(s).is_some() || s.trim_start().starts_with('#');
        let next_is_heading = is_heading(&w[1].1);
        let cur_is_heading = is_heading(&w[0].1);
        // 标题行强制独段；间距超阈值则分段
        if cur_is_heading || next_is_heading || gap > merge_threshold {
            out.push(std::mem::take(&mut cur));
            cur = w[1].1.clone();
        } else {
            // 同段：行间无空行合并（MinerU _merge_para_text 对齐）
            // 不加空格——中文行末无空格，英文连字符已在 postprocess_lines 处理
            cur.push_str(&w[1].1);
        }
    }
    out.push(cur);
    out
}

/// 行级后处理：西文连字符合并 + 全角 ASCII 归一化。
///
/// 借鉴 MinerU `merge_para_with_text`/`full_to_half_exclude_marks`：
/// - 行尾 ASCII 连字符 + 下行以小写字母开头 → 合并断词（如 "mainten-" + "ance" → "maintenance"）。
/// - 全角数字/字母 → 半角（０-９→0-9，Ａ-Ｚ→A-Z，ａ-ｚ→a-z）；中文全角标点保留。
pub fn postprocess_lines(lines: Vec<String>) -> Vec<String> {
    merge_hyphenated_lines(lines)
        .into_iter()
        .map(|l| normalize_full_width_ascii(&l))
        .collect()
}

/// 西文连字符合并：行尾 `-` 且下一行以小写字母开头时，去连字符拼接（无空格）。
fn merge_hyphenated_lines(lines: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut iter = lines.into_iter().peekable();
    while let Some(mut cur) = iter.next() {
        loop {
            let Some(next) = iter.peek() else { break };
            let cur_trim = cur.trim_end();
            let Some(base) = cur_trim.strip_suffix('-') else {
                break;
            };
            let nxt = next.trim_start();
            let Some(c) = nxt.chars().next() else { break };
            if !c.is_ascii_lowercase() {
                break;
            }
            cur = format!("{base}{nxt}");
            iter.next();
        }
        out.push(cur);
    }
    out
}

/// 全角数字/字母 → 半角（保留中文全角标点，如 （）《》…）。
fn normalize_full_width_ascii(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let cp = c as u32;
        let half = match cp {
            0xFF10..=0xFF19 => Some((cp - 0xFF10) as u8 + b'0'), // ０-９
            0xFF21..=0xFF3A => Some((cp - 0xFF21) as u8 + b'A'), // Ａ-Ｚ
            0xFF41..=0xFF5A => Some((cp - 0xFF41) as u8 + b'a'), // ａ-ｚ
            _ => None,
        };
        match half {
            Some(b) => out.push(b as char),
            None => out.push(c),
        }
    }
    out
}

/// MinerU 式标题级别推断（编号启发式，跳过 LLM）。
///
/// - 编号前缀：`1` / `2.1` / `2.1.1` / `一、` / `（1）` 等 → 级别 = 点分段数+1
///   （“1”→2，“2.1”→3，“2.1.1”→4），clamp 2..=6；
/// - 无编号的关键词小节：ABSTRACT/INTRODUCTION/REFERENCES/REFERENCE → 2；
/// - 其余 → `None`。
pub fn title_level(text: &str) -> Option<usize> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    // 无编号固定小节标题
    let up = t.to_uppercase();
    if matches!(
        up.as_str(),
        "ABSTRACT" | "INTRODUCTION" | "REFERENCES" | "REFERENCE"
    ) {
        return Some(2);
    }
    // 编号前缀；编号后须跟标题文本，纯编号行不算标题
    let (dots, rest) = parse_numbering(t)?;
    if rest.trim().is_empty() {
        return None;
    }
    Some((dots + 2).clamp(2, 6))
}

/// 解析行首编号前缀，返回 `(点分隔个数, 其后标题文本)`。
/// 依次尝试：ASCII 点分数字 → 中文数字（可带全角括号）→ 括号内数字。
fn parse_numbering(t: &str) -> Option<(usize, &str)> {
    let cs: Vec<char> = t.chars().collect();
    let n = cs.len();
    if n == 0 {
        return None;
    }
    let byte_at = |i: usize| t.char_indices().nth(i).map(|(b, _)| b);
    let rest_at = |i: usize| -> &str {
        match byte_at(i) {
            Some(b) => &t[b..],
            None => "",
        }
    };

    // 1) ASCII 点分数字：1 / 2.1 / 2.1.1
    if cs[0].is_ascii_digit() {
        let mut i = 0;
        while i < n && cs[i].is_ascii_digit() {
            i += 1;
        }
        let mut dots = 0usize;
        while i + 1 < n && cs[i] == '.' && cs[i + 1].is_ascii_digit() {
            dots += 1;
            i += 1;
            while i < n && cs[i].is_ascii_digit() {
                i += 1;
            }
        }
        let k = skip_sep_ws(&cs, i);
        return Some((dots, rest_at(k)));
    }

    // 2) 中文数字，可带全角括号：（一）/ 一、/ 一
    if cs[0] == '（' {
        let mut j = 1;
        let mut cnt = 0;
        while j < n && is_cn_numeral(cs[j]) {
            j += 1;
            cnt += 1;
        }
        if cnt > 0 {
            if j < n && (cs[j] == '）' || cs[j] == ')') {
                j += 1;
            }
            let k = skip_sep_ws(&cs, j);
            return Some((0, rest_at(k)));
        }
    } else if is_cn_numeral(cs[0]) {
        let mut j = 1;
        while j < n && is_cn_numeral(cs[j]) {
            j += 1;
        }
        if j < n && (cs[j] == '）' || cs[j] == ')') {
            j += 1;
        }
        let k = skip_sep_ws(&cs, j);
        return Some((0, rest_at(k)));
    }

    // 3) 括号内数字：(1) / （1）
    if cs[0] == '(' || cs[0] == '（' {
        let close = if cs[0] == '(' { ')' } else { '）' };
        let mut j = 1;
        let mut cnt = 0;
        while j < n && cs[j].is_ascii_digit() {
            j += 1;
            cnt += 1;
        }
        if cnt > 0 && j < n && cs[j] == close {
            j += 1;
            let k = skip_sep_ws(&cs, j);
            return Some((0, rest_at(k)));
        }
    }

    None
}

/// 跳过编号后的空白与可选分隔符（`\s*[.、．]?\s*`）。
fn skip_sep_ws(cs: &[char], mut i: usize) -> usize {
    while i < cs.len() && cs[i].is_whitespace() {
        i += 1;
    }
    if i < cs.len() && matches!(cs[i], '.' | '、' | '．') {
        i += 1;
    }
    while i < cs.len() && cs[i].is_whitespace() {
        i += 1;
    }
    i
}

fn is_cn_numeral(c: char) -> bool {
    matches!(
        c,
        '一' | '二' | '三' | '四' | '五' | '六' | '七' | '八' | '九' | '十'
    )
}

#[cfg(test)]
mod tests {
    use super::{merge_into_paragraphs, order_structure, order_text_regions, order_within_block};
    use crate::region::Region;
    use oar_ocr::domain::TextRegion;
    use oar_ocr::domain::structure::{LayoutElement, LayoutElementType, StructureResult};
    use oar_ocr::processors::BoundingBox;

    /// 构造区域：(x_min, x_max, y_min, y_max=+10, 文本)
    fn reg(x0: f32, x1: f32, y0: f32, t: &str) -> Region {
        Region::new(x0, x1, y0, y0 + 10.0, t)
    }

    /// 构造 TextRegion：(x_min, y_min, x_max, y_max, 文本)
    fn tr(x0: f32, y0: f32, x1: f32, y1: f32, text: &str) -> TextRegion {
        TextRegion {
            bounding_box: BoundingBox::from_coords(x0, y0, x1, y1),
            text: Some(text.into()),
            ..TextRegion::new(BoundingBox::from_coords(x0, y0, x1, y1))
        }
    }

    /// 构造 layout 块元素：(x_min, y_min, x_max, y_max, 类型, order_index)
    fn block_el(
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        ty: LayoutElementType,
        order: Option<u32>,
    ) -> LayoutElement {
        let mut el = LayoutElement::new(BoundingBox::from_coords(x0, y0, x1, y1), ty, 0.9);
        el.order_index = order;
        el
    }

    /// Region 从 TextRegion 转换（测试辅助）。
    fn regions_of(trs: &[TextRegion]) -> Vec<Region> {
        trs.iter()
            .filter_map(|r| {
                r.text.as_ref().filter(|t| !t.trim().is_empty()).map(|t| {
                    let b = &r.bounding_box;
                    Region::new(
                        b.x_min(),
                        b.x_max(),
                        b.y_min(),
                        b.y_max(),
                        t.as_ref().to_string(),
                    )
                })
            })
            .collect()
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

    /// 回归（9001c 文字版 4.1/4.2 正文缺行根因）：单栏页行宽天然变化（短标签 +
    /// 通栏段落），最大 center_x 间隙是相邻两行的宽度差，通栏正文区域跨过该
    /// "假间隙" → 不得判为双列（bridging 判别）。此前误判双列导致正文颠序/丢失。
    #[test]
    fn single_column_full_width_lines_do_not_split() {
        // 短标签行 cx≈100，通栏段落行 cx≈300（跨 75..540 全宽）
        let regions = vec![
            reg(75.0, 540.0, 500.0, "通栏正文一"), // cx=307，跨假间隙
            reg(75.0, 180.0, 400.0, "短标签"),     // cx=127
            reg(75.0, 540.0, 300.0, "通栏正文二"), // cx=307
            reg(75.0, 540.0, 200.0, "通栏正文三"), // cx=307
            reg(75.0, 160.0, 100.0, "短标题4.1"),  // cx=117
        ];
        // 最大 cx 间隙在 127 与 307 之间（gap=180，>3% 页宽），但通栏行跨过该
        // 中线 → bridging → 非列 → 单栏 y 排序，正文不被切割/颠倒。
        assert_eq!(
            order_text_regions(&regions),
            vec![
                "短标题4.1",
                "通栏正文三",
                "通栏正文二",
                "短标签",
                "通栏正文一"
            ],
            "单栏页不得误判双列"
        );
    }

    #[test]
    fn pdf_y_coordinates_flipped_sort_top_down() {
        // PDF 坐标原点左下：y 大=靠上。翻转 y（-y）后排序应仍上→下。
        let regions = vec![
            reg(50.0, 450.0, -300.0, "top"),
            reg(50.0, 450.0, -100.0, "bottom"),
            reg(50.0, 450.0, -200.0, "middle"),
        ];
        assert_eq!(
            order_text_regions(&regions),
            vec!["top", "middle", "bottom"]
        );
    }

    #[test]
    fn hyphen_merge_joins_broken_words() {
        use super::postprocess_lines;
        // "mainten-" + "ance" → "maintenance"；无连字符行不动
        let lines = vec!["mainten-".into(), "ance done".into(), "hello".into()];
        assert_eq!(postprocess_lines(lines), vec!["maintenance done", "hello"]);
        // 行尾连字符但下行大写开头（如专名/句首）不合并
        let lines = vec!["well-".into(), "Known".into()];
        assert_eq!(postprocess_lines(lines), vec!["well-", "Known"]);
    }

    #[test]
    fn full_width_ascii_normalized_half_width() {
        use super::postprocess_lines;
        // 全角数字/字母转半角；中文全角标点保留
        let lines = vec!["第１期（总第５７７期）ＡＢＣａｂｃ".into()];
        assert_eq!(postprocess_lines(lines), vec!["第1期（总第577期）ABCabc"]);
    }

    #[test]
    fn cn_numeral_halfwidth_paren_title() {
        use super::title_level;
        // 中文数字 + 半角 ) ：一) 小节 → 编号启发式命中 → 级别 2（C3）
        assert_eq!(title_level("一) 术语和定义"), Some(2));
        // 全角 ）仍命中（回归）
        assert_eq!(title_level("一）范围"), Some(2));
    }

    #[test]
    fn title_level_numbering_heuristic() {
        use super::title_level;
        // 编号层级 → 级别；"1"→2，"2.1"→3，"2.1.1"→4
        assert_eq!(title_level("1 Introduction"), Some(2));
        assert_eq!(title_level("2.1 Method"), Some(3));
        assert_eq!(title_level("2.1.1 x"), Some(4));
        assert_eq!(title_level("一、引言"), Some(2));
        assert_eq!(title_level("（1）xx"), Some(2));
        // 无编号关键词小节
        assert_eq!(title_level("ABSTRACT"), Some(2));
        // 普通正文句子 → 无级别
        assert_eq!(title_level("这是正文句子。"), None);
    }

    /// ADR-0009 D1：有 order_index 时块驱动排序，噪声块被跳过。
    #[test]
    fn block_driven_orders_by_order_index_skips_noise() {
        // 页面：Header 块（噪声）+ 两个 Text 块（order_index 1 在下、0 在上）
        let page = StructureResult {
            layout_elements: vec![
                block_el(0.0, 0.0, 1000.0, 50.0, LayoutElementType::Header, Some(0)),
                block_el(50.0, 100.0, 950.0, 150.0, LayoutElementType::Text, Some(2)),
                block_el(50.0, 200.0, 950.0, 250.0, LayoutElementType::Text, Some(1)),
            ],
            text_regions: Some(vec![
                tr(0.0, 10.0, 1000.0, 40.0, "页眉噪声"),
                tr(50.0, 110.0, 950.0, 140.0, "正文A"),
                tr(50.0, 210.0, 950.0, 240.0, "正文B"),
            ]),
            ..StructureResult::new("t", 0)
        };
        let regions = regions_of(page.text_regions.as_ref().unwrap());
        let out = order_structure(&page, &regions);
        // 正文B（order=1）先于 正文A（order=2）；页眉噪声被过滤
        assert_eq!(out, vec!["正文B", "正文A"]);
    }

    /// ADR-0009 D2：order_index 全 None → 降级到 RegionBlock；都无 → order_text_regions。
    #[test]
    fn block_driven_fallback_when_no_order_index() {
        // 无 order_index、无 region_blocks → 降级到 order_text_regions（单列 y 排序）
        let page = StructureResult {
            layout_elements: vec![
                block_el(50.0, 200.0, 950.0, 250.0, LayoutElementType::Text, None),
                block_el(50.0, 100.0, 950.0, 150.0, LayoutElementType::Text, None),
            ],
            text_regions: Some(vec![
                tr(50.0, 210.0, 950.0, 240.0, "下"),
                tr(50.0, 110.0, 950.0, 140.0, "上"),
            ]),
            region_blocks: None,
            ..StructureResult::new("t", 0)
        };
        let regions = regions_of(page.text_regions.as_ref().unwrap());
        let out = order_structure(&page, &regions);
        // y 排序：上 先于 下
        assert!(out[0].contains("上"));
        assert!(out[1].contains("下"));
    }

    /// ADR-0009 D3：块内段落合并——相邻行 y 间距小 → 合并为一段。
    #[test]
    fn merge_into_paragraphs_joins_close_lines() {
        // y=100,110,120 间距 10（小）→ 合一段；y=200 间距 80（大）→ 分段
        let lines = vec![
            (100.0, "第一行".into()),
            (110.0, "第二行".into()),
            (120.0, "第三行".into()),
            (200.0, "第二段".into()),
        ];
        let out = merge_into_paragraphs(&lines);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], "第一行第二行第三行");
        assert_eq!(out[1], "第二段");
    }

    /// ADR-0009 D3：标题行（# 开头）强制独段，不与相邻行合并。
    #[test]
    fn merge_into_paragraphs_heading_standalone() {
        let lines = vec![(100.0, "# 标题".into()), (110.0, "正文".into())];
        let out = merge_into_paragraphs(&lines);
        assert_eq!(out, vec!["# 标题", "正文"]);
    }

    /// ADR-0009 Q6：单 Text 块裹双列 → 块内列检测分离左右列。
    #[test]
    fn order_within_block_splits_two_columns() {
        // 单块内 6 regions：左列 3 行 + 右列 3 行，x 中心分两簇
        let regions = vec![
            Region::new(50.0, 450.0, 100.0, 110.0, "L1"),
            Region::new(50.0, 450.0, 200.0, 210.0, "L2"),
            Region::new(50.0, 450.0, 300.0, 310.0, "L3"),
            Region::new(550.0, 950.0, 100.0, 110.0, "R1"),
            Region::new(550.0, 950.0, 200.0, 210.0, "R2"),
            Region::new(550.0, 950.0, 300.0, 310.0, "R3"),
        ];
        let out = order_within_block(&regions);
        let texts: Vec<&str> = out.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(texts, vec!["L1", "L2", "L3", "R1", "R2", "R3"]);
    }
}
