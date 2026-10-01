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

/// 行首列表 marker 后的**空格恢复**（#15 尾巴）。
///
/// OCR rec 常把 `a) 内容` 的 marker 后空格吃掉，且**同页有/无空格混出**
/// （GJB 9001C 页 24 实测：`a)法律法规要求；` 与 `2) 确保...` 并存）。
/// MinerU rec 保留空格，且标准文档排版原文 marker 后必有空格——按原文恢复。
///
/// 判据与 [`starts_with_list_marker`] 的字母**括号式**同形态（单 ASCII 字母 +
/// `)` `）`），**刻意收窄**：
/// - 不做数字式（`1)` 与标题编号冲突的既有立场不变，见模块注释）；
/// - 不做点式/顿号式（`A.1` 附录编号、`A、总则` 顿号标题语义模糊，风险大）；
/// - marker 后一位必须是**非空白、非数字**才补（已有空格的 `2) 确保` 与
///   `a)1 项` 紧凑编号都不动）。
///
/// 行中出现的 `见a)条款` 不受影响（只在 trim_start 后的行首判定）；前导
/// 空白原样保留。
pub fn restore_marker_space(text: &str) -> String {
    let t = text.trim_start();
    let mut it = t.chars();
    let Some(c0) = it.next() else {
        return text.to_string();
    };
    if !c0.is_ascii_alphabetic() {
        return text.to_string();
    }
    let Some(c1) = it.next() else {
        return text.to_string();
    };
    if !matches!(c1, ')' | '）') {
        return text.to_string();
    }
    let Some(c2) = it.next() else {
        return text.to_string();
    };
    if c2.is_whitespace() || c2.is_numeric() {
        return text.to_string();
    }
    // marker 段（前导空白 + c0 + c1）+ 空格 + 其余。c0 是 ASCII 恒 1 字节。
    let prefix_len = text.len() - t.len();
    format!("{}{}{} {}", &text[..prefix_len], c0, c1, &t[1 + c1.len_utf8()..])
}

/// marker 形态 → 是否 MinerU 意义上的 **ordered**（#10 切片 4）。
///
/// 对齐 `docvortex/render/_internal/common/list_items.py::parse_list_item_marker`
/// 的 kind 分类：字母点式 `a.` `A.`（`[A-Za-z]\.`）是 `ordered`；bullet `-*+` 是
/// `unordered`；字母括号式 `a)` 是 `explicit`；中文顿号式 `A、` 与中文括号式
/// `（一）` MinerU 不识别（kind=none）。`infer_list_attribute` 的投票规则是
/// 「最浅层叶子 kind 全 `ordered` → `"ordered"`，否则 `"unordered"`」——
/// explicit/none 都归 unordered。故本函数只把字母点式记 `Some(true)`，
/// 其余 marker 记 `Some(false)`，非 marker 行 `None`（不参与投票）。
pub fn marker_is_ordered(line: &str) -> Option<bool> {
    if !starts_with_list_marker(line) {
        return None;
    }
    let t = line.trim_start();
    let c0 = t.chars().next()?;
    if c0.is_ascii_alphabetic() {
        // a. → ordered；a) a） a、 A，→ explicit → unordered
        return Some(matches!(t[1..].chars().next(), Some('.')));
    }
    Some(false) // bullet / 中文括号式
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

    /// #10 切片 4：marker 形态 → ordered 投票（`a.` 是唯一 ordered 组）。
    #[test]
    fn marker_is_ordered_cases() {
        assert_eq!(marker_is_ordered("c.附录"), Some(true));
        assert_eq!(marker_is_ordered("A. General"), Some(true));
        assert_eq!(marker_is_ordered("f)  确定产品通用化要求"), Some(false), "explicit → unordered");
        assert_eq!(marker_is_ordered("A、总则"), Some(false), "中文顿号 MinerU 不识别 → unordered");
        assert_eq!(marker_is_ordered("（一）理解组织及其环境"), Some(false));
        assert_eq!(marker_is_ordered("- 引导启动项"), Some(false));
        assert_eq!(marker_is_ordered("1. 数字式不做"), None, "数字式不在本仓 marker 集");
        assert_eq!(marker_is_ordered("普通正文行"), None);
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

    /// #15 尾巴：marker 后空格恢复（rec 丢空格的行首字母括号式）。
    #[test]
    fn restore_marker_space_cases() {
        // 命中：rec 丢空格的字母括号式（GJB 页 24 实测形态）
        assert_eq!(restore_marker_space("a)法律法规要求；"), "a) 法律法规要求；");
        assert_eq!(restore_marker_space("d）顾客要求："), "d） 顾客要求：");
        assert_eq!(restore_marker_space("  f)对交付后活动采取以下控制措施："), "  f) 对交付后活动采取以下控制措施：", "前导空白保留");
        // 已有空格 / 行尾：不动
        assert_eq!(restore_marker_space("2) 确保与产品使用和维护相关的技术文件得到控制和更新；"), "2) 确保与产品使用和维护相关的技术文件得到控制和更新；");
        assert_eq!(restore_marker_space("b)"), "b)");
        // 收窄域：点式/顿号式/数字式不补（附录编号与标题编号域）
        assert_eq!(restore_marker_space("A.1 结构和术语"), "A.1 结构和术语");
        assert_eq!(restore_marker_space("A、总则"), "A、总则");
        assert_eq!(restore_marker_space("1)按规定完成产品使用和维修的技术培训；"), "1)按规定完成产品使用和维修的技术培训；", "数字式不做");
        // marker 后是数字（a)1 项）与行中出现：不动
        assert_eq!(restore_marker_space("a)1 项"), "a)1 项");
        assert_eq!(restore_marker_space("详见a)条款"), "详见a)条款");
        // 非 marker 开头 / 空串
        assert_eq!(restore_marker_space("（一）理解组织及其环境"), "（一）理解组织及其环境");
        assert_eq!(restore_marker_space(""), "");
    }
}
