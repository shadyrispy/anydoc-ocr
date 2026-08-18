//! 列表项前缀识别（T6）：OCR 通路专用。
//!
//! 问题：det 常把列表项拆成「孤立前缀窄条 region + 内容宽条 region」——
//! `a) 本部分...` 是整条（前缀+内容同 region），而 `b)` 是孤立前缀、内容在
//! 另一个 region。阅读顺序/段落合并不做配对 → 输出 `b)` 空行、内容游离。
//!
//! 本模块只做**识别**（纯函数、可单测），配对重组在 `gfm_adapter` 消费。
//! 保守策略：只识别字母括号式 `a)` `b）` 与中文括号式 `（一）（二）`、bullet
//! `-` `•`；**不做数字式**（`1.` 与标题/编号冲突，`title_level` 已处理标题）。

/// 孤立列表前缀识别：该行**只有 marker、没有内容**（极短），且是列表前缀。
///
/// - 字母括号式：`a)` `b）` `c.`（1 字母 + 括号/句点，≤3 字符）
/// - 中文括号式：`（一）（二）`（≤6 字符）
/// - bullet：`-` `•` `·` `*`（≤2 字符，排除 `#` 标题）
///
/// 排除：`#` 开头（已加标题前缀）、空行、超长行（有内容）。
pub fn is_isolated_marker(line: &str) -> bool {
    let t = line.trim();
    if t.is_empty() || t.starts_with('#') {
        return false;
    }
    let n = t.chars().count();
    if n > 6 {
        return false;
    }
    let c: Vec<char> = t.chars().collect();
    // bullet：单字符 - • · *（n<=2 已保证，直接判断）
    if matches!(c[0], '-' | '•' | '·' | '*') {
        return true;
    }
    // 字母括号式：a) a） a. a、
    if n <= 3 && c[0].is_ascii_alphabetic() {
        return matches!(c.get(1), Some(')') | Some('）') | Some('.') | Some('、') | Some('，') | Some(','));
    }
    // 中文括号式：（一）（二）（1）
    if c[0] == '（' && *c.last().unwrap() == '）' && n >= 3 {
        let inner: String = c[1..n - 1].iter().collect();
        if inner.chars().all(|ch| ch.is_numeric() || is_cn_numeral(ch)) && !inner.is_empty() {
            return true;
        }
    }
    false
}

fn is_cn_numeral(c: char) -> bool {
    matches!(c, '一' | '二' | '三' | '四' | '五' | '六' | '七' | '八' | '九' | '十' | '〇' | '零')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letter_marker_isolated() {
        assert!(is_isolated_marker("b)"));
        assert!(is_isolated_marker("b）"));
        assert!(is_isolated_marker("c."));
        assert!(is_isolated_marker("A、"));
        assert!(is_isolated_marker("  d)  "), "允许首尾空白");
    }

    #[test]
    fn cn_marker_isolated() {
        assert!(is_isolated_marker("（一）"));
        assert!(is_isolated_marker("（二）"));
        assert!(is_isolated_marker("（10）"));
        assert!(is_isolated_marker("（十二）"));
    }

    #[test]
    fn bullet_marker_isolated() {
        assert!(is_isolated_marker("-"));
        assert!(is_isolated_marker("•"));
        assert!(is_isolated_marker("·"));
    }

    #[test]
    fn content_lines_not_markers() {
        assert!(!is_isolated_marker("b) 本部分强调规范的内容"));
        assert!(!is_isolated_marker("本部分强调规范的内容"));
        assert!(!is_isolated_marker(""));
        assert!(!is_isolated_marker("   "));
        assert!(!is_isolated_marker("# 标题"), "标题行排除");
    }

    #[test]
    fn numeric_marker_not_treated() {
        // 数字式与标题/编号冲突，保守不做（标题由 title_level 处理）
        assert!(!is_isolated_marker("1."));
        assert!(!is_isolated_marker("1)"));
        assert!(!is_isolated_marker("4.2"));
    }

    #[test]
    fn long_lines_not_markers() {
        assert!(!is_isolated_marker("abcdefg"), ">6 字符视为有内容");
    }
}
