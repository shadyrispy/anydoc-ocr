//! 行级后处理与段落合并。
//!
//! - 段落合并（`merge_into_paragraphs`）：块内相邻行按行距合并，对齐 MinerU
//!   `_merge_para_text`；
//! - 行级后处理（`postprocess_lines`）：西文连字符合并 + 全角 ASCII 归一化。

use super::title::title_level;

/// ADR-0009 D3：段落合并——相邻行 y 间距 < 行高 1.5x → 同段。
///
/// 对齐 MinerU `_merge_para_text`：行间无空行（间距小）则合并为一段。
/// 行高用块内中位 region 高度估计；空行/标题行不参与合并。
pub(super) fn merge_into_paragraphs(lines: &[(f32, String)]) -> Vec<String> {
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

#[cfg(test)]
mod tests {
    use super::{merge_into_paragraphs, postprocess_lines};

    #[test]
    fn hyphen_merge_joins_broken_words() {
        // "mainten-" + "ance" → "maintenance"；无连字符行不动
        let lines = vec!["mainten-".into(), "ance done".into(), "hello".into()];
        assert_eq!(postprocess_lines(lines), vec!["maintenance done", "hello"]);
        // 行尾连字符但下行大写开头（如专名/句首）不合并
        let lines = vec!["well-".into(), "Known".into()];
        assert_eq!(postprocess_lines(lines), vec!["well-", "Known"]);
    }

    #[test]
    fn full_width_ascii_normalized_half_width() {
        // 全角数字/字母转半角；中文全角标点保留
        let lines = vec!["第１期（总第５７７期）ＡＢＣａｂｃ".into()];
        assert_eq!(postprocess_lines(lines), vec!["第1期（总第577期）ABCabc"]);
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
}
