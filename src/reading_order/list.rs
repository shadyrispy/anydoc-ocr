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
/// 排除：`#` 开头（标题行）、空行、超长行（有内容）。
/// 调用方传入**渲染视图**（`Region::rendered_line`，#6 第 2 步）——标题行在
/// 本函数看到的是字面量 `#`，与旧"producer 已写前缀"的口径一致。
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

/// 行首列表标记识别（#10 切片 1）：该行**以列表 marker 开头**（可带内容）。
///
/// 与 [`is_isolated_marker`] 同风格、同保守度：字母括号式 `a)` `b）` `A、`、
/// 中文括号式 `（一）（12）`、bullet `-` `•` `·` `*`；**不做裸数字式**
/// （`1.` `1)` 与标题编号冲突，标题由 `title_level` 处理——既有立场不变）。
///
/// 消费方：`merge_into_paragraphs` 的强制独段护栏——列表项不并入相邻段落
/// （GJB 9001C 真实样本实测 `f)/g)/h)` 被并成 705 字大段）。误伤面评估：
/// 字母点式 `A. ` 行首（英文缩写人名类）罕见，且独段只是不合并、不丢内容。
pub fn starts_with_list_marker(line: &str) -> bool {
    let t = line.trim_start();
    let mut it = t.chars();
    let Some(c0) = it.next() else {
        return false;
    };
    if c0 == '#' {
        return false;
    }
    // bullet：- • · *（后随空白或整行就一个字符）
    if matches!(c0, '-' | '•' | '·' | '*') {
        return it.next().is_none_or(|c| c.is_whitespace());
    }
    // 字母括号式：a) a） a. a、 A，——不要求后随空格（OCR 常丢空格）
    if c0.is_ascii_alphabetic() {
        return matches!(it.next(), Some(')') | Some('）') | Some('.') | Some('、') | Some('，') | Some(','));
    }
    // 中文括号式：（一）（12）——全角开括号起头，内芯纯数字/中文数字
    if c0 == '（' {
        return match t.find('）') {
            Some(close) => {
                let inner: String = t['（'.len_utf8()..close].chars().collect();
                !inner.is_empty()
                    && inner.chars().all(|ch| ch.is_numeric() || is_cn_numeral(ch))
            }
            None => false,
        };
    }
    false
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

    /// #10 切片 1：行首标记（可带内容）——消费方是 merge 独段护栏。
    #[test]
    fn starts_with_list_marker_cases() {
        assert!(starts_with_list_marker("f)  确定产品通用化要求"));
        assert!(starts_with_list_marker("a）提供资源"));
        assert!(starts_with_list_marker("A、总则"));
        assert!(starts_with_list_marker("c.附录"));
        assert!(starts_with_list_marker("- 引导启动项"));
        assert!(starts_with_list_marker("• 要点"));
        assert!(starts_with_list_marker("-"), "孤立 bullet 同样命中");
        assert!(starts_with_list_marker("（一）理解组织及其环境"));
        assert!(starts_with_list_marker("（12）试验方法"));
        // 数字式不做（标题编号域，title_level 负责）
        assert!(!starts_with_list_marker("1. 数字式不做"));
        assert!(!starts_with_list_marker("1) 同上"));
        assert!(!starts_with_list_marker("4.2 组织环境"));
        // 非标记
        assert!(!starts_with_list_marker("普通正文行"));
        assert!(!starts_with_list_marker("# 标题"));
        assert!(!starts_with_list_marker(""));
        assert!(!starts_with_list_marker("（参见第 4 章）"), "括号内非纯数字");
    }
}
