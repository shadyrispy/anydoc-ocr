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

    // 正文区域：(中心x, y, 文本)；跨整页元素：(y, 文本)
    let mut left: Vec<(f32, String)> = regions
        .iter()
        .filter(|r| !r.is_full_width(page_w) && r.center_x() < split)
        .map(|r| (r.y_min, r.text.clone()))
        .collect();
    let mut right: Vec<(f32, String)> = regions
        .iter()
        .filter(|r| !r.is_full_width(page_w) && r.center_x() >= split)
        .map(|r| (r.y_min, r.text.clone()))
        .collect();
    left.sort_by(ord_y);
    right.sort_by(ord_y);

    // 整宽元素按 y 归页眉(y<正文起点)/页脚(y>正文终点)/正文区间(罕见置后)
    let full: Vec<(f32, String)> = regions
        .iter()
        .filter(|r| r.is_full_width(page_w))
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
    out
}

/// 检测双列切分线（gutter）。返回 `None` 表示单栏/无法切分。
///
/// 列检测用**所有**正文区域（含宽条目）的中心 x，取最大间隙切分；要求间隙
/// >= 3% 页宽且两侧各 >=2 区域，避免把单栏内的大间距误判为分栏。真正跨整页
/// （x 同时贴近左右边距）的元素（页眉/页脚/通栏标题）先剔除。
pub fn detect_column_split(regions: &[Region]) -> Option<f32> {
    if regions.len() < 4 {
        return None;
    }
    let page_w = Region::page_w(regions);
    if page_w <= 0.0 {
        return None;
    }
    let mut body: Vec<f32> = regions
        .iter()
        .filter(|r| !r.is_full_width(page_w))
        .map(|r| r.center_x())
        .collect();
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
    (best_gap >= min_gap && best_i >= 2 && (body.len() - best_i) >= 2)
        .then(|| (body[best_i - 1] + body[best_i]) / 2.0)
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
            let Some(base) = cur_trim.strip_suffix('-') else { break };
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
    if matches!(up.as_str(), "ABSTRACT" | "INTRODUCTION" | "REFERENCES" | "REFERENCE") {
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
    matches!(c, '一' | '二' | '三' | '四' | '五' | '六' | '七' | '八' | '九' | '十')
}

#[cfg(test)]
mod tests {
    use crate::region::Region;
    use super::order_text_regions;

    /// 构造区域：(x_min, x_max, y_min, y_max=+10, 文本)
    fn reg(x0: f32, x1: f32, y0: f32, t: &str) -> Region {
        Region::new(x0, x1, y0, y0 + 10.0, t)
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

    #[test]
    fn pdf_y_coordinates_flipped_sort_top_down() {
        // PDF 坐标原点左下：y 大=靠上。翻转 y（-y）后排序应仍上→下。
        let regions = vec![
            reg(50.0, 450.0, -300.0, "top"),
            reg(50.0, 450.0, -100.0, "bottom"),
            reg(50.0, 450.0, -200.0, "middle"),
        ];
        assert_eq!(order_text_regions(&regions), vec!["top", "middle", "bottom"]);
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
}
