//! 文本健康检查：乱码字符检测（PDF / OFD 共用）+ 标题前缀注入（三通路统一）。
//!
//! 坏字符三类：U+FFFD 替换符、私有区 U+E000..=U+F8FF、控制字符。判定阈值
//! （`min_total` / `bad_percent`）由本模块的
//! [`GARBLED_MIN_TOTAL_CHARS`] / [`GARBLED_BAD_PERCENT_THRESHOLD`] 统一提供，
//! PDF 与 OFD 共用同一阈值（50 字符 / 20% 占比），常量集中于此，防异名漂移。

use crate::reading_order;

/// 标题前缀最大行宽：超过此字符数视为正文，不加标题前缀（编号启发式分支）。
pub const TITLE_MAX_CHARS: usize = 60;

/// 整页坏字符占比与总量阈值（GARBLED 判乱码，PDF/OFD 共用，防异名漂移）。
pub const GARBLED_MIN_TOTAL_CHARS: usize = 50;
pub const GARBLED_BAD_PERCENT_THRESHOLD: usize = 20;

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

/// 剥掉行首/行尾的**行内样式标记**（`ANYDOC_RICH_TEXT` 注入的 `**`/`<u>`/`<s>`
/// 及闭合），仅用于标题启发式的**判定视图**——前缀注入仍作用于原始行，`#`
/// 前缀检查也走原始行。
///
/// 只认这三类**成对且非歧义**的标记，故意不含裸 `*斜体*`（`*` 在正文/脚注里
/// 常见，误剥风险大于收益）。只做首尾成对剥除（内部标记不动），迭代至不动点；
/// 不成对的杂散标记原样保留，判定退化为"无标题"，不会误加前缀。
pub fn strip_inline_style_markers(line: &str) -> String {
    const PAIRS: [(&str, &str); 3] = [("**", "**"), ("<u>", "</u>"), ("<s>", "</s>")];
    let mut s = line.trim().to_string();
    // 每轮尝试剥一对首尾标记；剥得动就继续，剥不动即收敛。
    loop {
        let mut stripped: Option<String> = None;
        for (open, close) in PAIRS {
            if s.len() >= open.len() + close.len()
                && s.starts_with(open)
                && s.ends_with(close)
            {
                // 标记全为 ASCII，字节切片安全
                stripped = Some(s[open.len()..s.len() - close.len()].trim().to_string());
                break;
            }
        }
        match stripped {
            Some(next) if next != s => s = next,
            _ => return s,
        }
    }
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
    apply_title_prefixes_styled(lines, title_hints, numbering, false)
}

/// 同 [`apply_title_prefixes`]，`styled = true` 时标题判定改在**剥除行内外
/// 样式标记后的视图**上做（`ANYDOC_RICH_TEXT` 通路的 `**一、总则**` 仍是标题，
/// 前缀注入在原始行 → `## **一、总则**`）；`styled = false` 逐字节等价旧行为
/// （golden 守护）。仅判定视图不同，产出规则全部不变。
pub fn apply_title_prefixes_styled(
    lines: &[String],
    title_hints: &[(String, usize)],
    numbering: bool,
    styled: bool,
) -> Vec<String> {
    let view = |line: &str| -> String {
        if styled { strip_inline_style_markers(line) } else { line.to_string() }
    };
    lines
        .iter()
        .map(|line| {
            if line.trim_start().starts_with('#') {
                return line.clone();
            }
            let v = view(line);
            if numbering
                && v.chars().count() <= TITLE_MAX_CHARS
                && let Some(lv) = reading_order::title_level(&v)
            {
                return format!("{} {}", "#".repeat(lv), line);
            }
            if !title_hints.is_empty() {
                let lt = v.trim();
                for (tt, lv) in title_hints {
                    if lt == tt.as_str() || lt.contains(tt.as_str()) || tt.contains(lt) {
                        return format!("{} {}", "#".repeat(*lv), line);
                    }
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
        assert!(!has_garbled_chars(format!("{good}{bad}").chars(), 50, 20));
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

    // ── ANYDOC_RICH_TEXT：样式标记感知的标题判定 ──

    #[test]
    fn strip_inline_style_markers_pairs_only() {
        // 首尾成对 **/<u>/<s> 剥除，嵌套迭代至不动点
        assert_eq!(strip_inline_style_markers("**一、总则**"), "一、总则");
        assert_eq!(strip_inline_style_markers("<u>重点</u>"), "重点");
        assert_eq!(strip_inline_style_markers("**<u>双重</u>**"), "双重");
        // 内部标记不动
        assert_eq!(strip_inline_style_markers("前**中**后"), "前**中**后");
        // 不成对/裸斜体（歧义）原样保留
        assert_eq!(strip_inline_style_markers("**未闭合"), "**未闭合");
        assert_eq!(strip_inline_style_markers("*斜体*"), "*斜体*");
        // 纯标记行剥后为空
        assert_eq!(strip_inline_style_markers("** **"), "");
    }

    #[test]
    fn styled_title_judgment_injects_on_original_line() {
        // styled=true：`**一、总则**` 判定视图命中编号启发式 → 前缀加在原始行，
        // 样式标记保留（MinerU text_evidence 的 style + heading 可共存语义）。
        let lines = vec!["**一、总则**".to_string(), "普通**正文**".to_string()];
        let out = apply_title_prefixes_styled(&lines, &[], true, true);
        assert_eq!(out[0], "## **一、总则**");
        assert_eq!(out[1], "普通**正文**");
        // styled=false：判定视图 = 原始行，`**一、总则**` 以 `*` 开头无编号命中
        // → 逐字节等价旧行为（不加前缀）。
        let legacy = apply_title_prefixes_styled(&lines, &[], true, false);
        assert_eq!(legacy, lines);
        // 已带 # 前缀的样式行仍跳过（防双重标记，检查走原始行）。
        let hashed = vec!["# **标题**".to_string()];
        assert_eq!(apply_title_prefixes_styled(&hashed, &[], true, true), hashed);
        // hints 匹配也走剥离视图
        let hint_lines = vec!["<u>附表清单</u>".to_string()];
        let hints = vec![("附表清单".to_string(), 3)];
        let out = apply_title_prefixes_styled(&hint_lines, &hints, false, true);
        assert_eq!(out[0], "### <u>附表清单</u>");
    }
}
