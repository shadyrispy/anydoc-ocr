//! MinerU 式标题级别推断（编号启发式，跳过 LLM）。

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
    use super::title_level;

    #[test]
    fn cn_numeral_halfwidth_paren_title() {
        // 中文数字 + 半角 ) ：一) 小节 → 编号启发式命中 → 级别 2（C3）
        assert_eq!(title_level("一) 术语和定义"), Some(2));
        // 全角 ）仍命中（回归）
        assert_eq!(title_level("一）范围"), Some(2));
    }

    #[test]
    fn title_level_numbering_heuristic() {
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
