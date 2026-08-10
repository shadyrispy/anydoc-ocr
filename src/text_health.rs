//! 文本健康检查：乱码字符检测（PDF / OFD 共用）+ 标题前缀注入（三通路统一）。
//!
//! 坏字符三类：U+FFFD 替换符、私有区 U+E000..=U+F8FF、控制字符。判定阈值
//! （`min_total` / `bad_percent`）由调用方各自保留——PDF 与 OFD 的常量略有差异，
//! 但字符分类逻辑完全一致，在此收敛为单一实现，避免两处漂移。

use crate::reading_order;

/// 标题前缀最大行宽：超过此字符数视为正文，不加标题前缀（编号启发式分支）。
pub const TITLE_MAX_CHARS: usize = 60;

/// 单字符是否为"坏字体"特征字符。
pub fn is_garbled_char(c: char) -> bool {
    let cp = c as u32;
    c == '\u{FFFD}' || (0xE000..=0xF8FF).contains(&cp) || c.is_control()
}

/// 统计字符流中的坏字符占比，命中阈值返回 `true`。
///
/// `total > min_total && bad * 100 >= total * bad_percent`。
/// `min_total` 防小页/空页误伤；`bad_percent` 为坏字符占比下限（如 20 表示 20%）。
pub fn has_garbled_chars(
    chars: impl Iterator<Item = char>,
    min_total: usize,
    bad_percent: usize,
) -> bool {
    let mut total = 0usize;
    let mut bad = 0usize;
    for c in chars {
        total += 1;
        if is_garbled_char(c) {
            bad += 1;
        }
    }
    total > min_total && bad * 100 >= total * bad_percent
}

/// 标题前缀注入（PDF 文字层 / OFD 文字层 / gfm OCR 三通路统一）。
///
/// 规则（按序，命中即返回、跳过后续）：
/// 1. 行已以 `#` 开头（trim 后）→ 保持原样（防双重标记）；
/// 2. `numbering` 为真且行 ≤ [`TITLE_MAX_CHARS`] 字符、且 `reading_order::title_level`
///    编号启发式命中 → 加 `"#".repeat(level) + " "`（PDF/OFD 文字层无布局标题信号，
///    仅此启发式；gfm 传 `numbering=false` 不抹平其布局驱动差异）；
/// 3. 命中 `title_hints`（`(文本, 级别)`，由布局模型或外部提供）→ 加对应级别前缀；
/// 4. 均不命中 → 保持原样。
///
/// `title_hints` 为空（PDF/OFD 文字层）+ `numbering=true` 即纯编号启发式；
/// `title_hints` 非空（gfm 布局标题）+ `numbering=false` 即纯布局驱动。
pub fn apply_title_prefixes(
    lines: &[String],
    title_hints: &[(String, usize)],
    numbering: bool,
) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            if line.trim_start().starts_with('#') {
                return line.clone();
            }
            if numbering
                && line.chars().count() <= TITLE_MAX_CHARS
                && let Some(lv) = reading_order::title_level(line)
            {
                return format!("{} {}", "#".repeat(lv), line);
            }
            let lt = line.trim();
            for (tt, lv) in title_hints {
                if lt == tt.as_str() || lt.contains(tt.as_str()) || tt.contains(lt) {
                    return format!("{} {}", "#".repeat(*lv), line);
                }
            }
            line.clone()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_bad_and_good_chars() {
        assert!(is_garbled_char('\u{FFFD}'));
        assert!(is_garbled_char('\u{E000}'));
        assert!(is_garbled_char('\u{F8FF}'));
        assert!(is_garbled_char('\u{0007}')); // 控制字符
        assert!(!is_garbled_char('a'));
        assert!(!is_garbled_char('中'));
        assert!(!is_garbled_char('，'));
    }

    #[test]
    fn threshold_and_ratio_gate() {
        // 60 个替换符 > 50 且占比 100% >= 20% → 乱码
        let s: String = "\u{FFFD}".repeat(60);
        assert!(has_garbled_chars(s.chars(), 50, 20));
        // 10 个替换符总量不足 50 → 不算（防小页误伤）
        let s: String = "\u{FFFD}".repeat(10);
        assert!(!has_garbled_chars(s.chars(), 50, 20));
        // 70 总字符中 10 个私有区（占比 ~14% < 20%）→ 不算
        let mut s = "正常正文".repeat(10);
        s.push('\u{E000}');
        // 重新构造 70 总 / 10 坏
        let bad: String = "\u{E000}".repeat(10);
        let good: String = "正".repeat(60);
        assert!(!has_garbled_chars(
            format!("{good}{bad}").chars(),
            50,
            20
        ));
    }

    #[test]
    fn numbering_only_when_flagged() {
        // numbering=true 空 hints：编号启发式命中加前缀；超长正文不变。
        let lines = vec![
            "一、总则".to_string(),
            "这是正文第一句。".to_string(),
            "1.1 适用范围".to_string(),
        ];
        let out = apply_title_prefixes(&lines, &[], true);
        assert_eq!(out[0], "## 一、总则");
        assert_eq!(out[1], "这是正文第一句。");
        assert_eq!(out[2], "### 1.1 适用范围");
        // numbering=false：即使编号也不加（gfm 布局驱动语义）
        let out = apply_title_prefixes(&lines, &[], false);
        assert_eq!(out, lines);
    }

    #[test]
    fn layout_hints_drive_prefix_without_numbering() {
        // 空 hints + numbering=false 时，靠传入的布局提示加前缀。
        let lines = vec!["究极标题".to_string(), "普通正文".to_string()];
        let hints = vec![("究极标题".to_string(), 2)];
        let out = apply_title_prefixes(&lines, &hints, false);
        assert_eq!(out[0], "## 究极标题");
        assert_eq!(out[1], "普通正文");
    }

    #[test]
    fn existing_hash_prefix_preserved() {
        let lines = vec!["# 已带前缀".to_string(), "一、小节".to_string()];
        let out = apply_title_prefixes(&lines, &[], true);
        assert_eq!(out[0], "# 已带前缀");
        assert_eq!(out[1], "## 一、小节");
    }
}
