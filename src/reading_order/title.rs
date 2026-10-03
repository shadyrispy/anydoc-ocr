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
    // 编号后必须紧跟标题词（字母/汉字/数字）。紧跟标点说明这是正文里被切断的
    // 数值行而非章条编号——如段落「…较上一年度增长 14.2%。其中…」在按行
    // 切分后，行首 `14.2` 会被点分编号启发式命中，`.` 后的 `2` 吃成第二节，
    // 剩下 `%。…` 当"标题文本"，于是一行普通数值被套上 `###` 并把段落截断。
    match rest.trim().chars().next() {
        Some(c) if c.is_alphanumeric() => {}
        _ => return None,
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
        // 首段数字 >= 4 位 → 年份/日期（GJB 封面「2017―05―18 发布」，MinerU
        // 4.0.8 判普通段落），不是章条编号：章条编号极少到四位数。
        if i >= 4 {
            return None;
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

    #[test]
    fn year_like_leading_number_is_not_numbering() {
        // GJB 封面「2017―05―18 发布」：MinerU 4.0.8 真 CLI 判普通段落，此前
        // ASCII 数字分支拿 `2017` 当编号 → 误造 `##`。四位数首段 = 日期/年份。
        assert_eq!(title_level("2017―05―18 发布"), None);
        assert_eq!(title_level("2024 年度报告"), None);
        // 三位及以下仍照旧（章条编号不会更长，但也不排除个别文档）
        assert_eq!(title_level("123 总则"), Some(2));
    }

    #[test]
    fn decimal_or_percent_line_start_is_not_numbering() {
        // 段落按行切断后行首只剩 `14.2%。…`：`14` 不足四位护栏，`.2` 被吃成
        // 第二节，剩下 `%。…` 当标题文本 → 数值行套上 `###` 并截断段落。
        // 复现：tests/samples/synth_samples.pdf 页1 文字层通路。
        assert_eq!(title_level("14.2%。其中第四季度营业收入达到"), None);
        assert_eq!(title_level("3.1%"), None);
        // 编号后紧跟标题词（中文/西文）仍照旧命中
        assert_eq!(title_level("2.1 分季度营收对比"), Some(3));
        assert_eq!(title_level("1 绪论"), Some(2));
        assert_eq!(title_level("2.1.1 细分"), Some(4));
    }

    /// #16 护栏的GJB 9001C 真实回归（GJB 全文 38 页，护栏前 173 标题 →护栏后 133，
    /// **零新增**；40 处全是误判）。真值：MinerU 4.0.8 `--tier basic -p 20-24`
    /// 对同一段落既不打 `#` 也不并入上级列表项——`1)` `2)` 各自独立成普通行。
    #[test]
    fn bracket_numbered_list_item_is_not_title() {
        // 旧版把这40 行套成 `##`（`## 1)  过程；`），且因「标题强制独段」把
        // 上行 `b)  建立下列内容的准则：` 截断——一个误判连带截断整段。
        assert_eq!(title_level("1)  过程；"), None);
        assert_eq!(title_level("2)  产品和服务的接收。"), None);
        assert_eq!(title_level("3)  确定是否存在或可能发生类似的不合格。"), None);
        // 半角/全角右括号、以及带尾随句号的形态一并覆盖（GJB 上两种都有）
        assert_eq!(title_level("1)外部供方的绩效。"), None);
        assert_eq!(title_level("2） 方法、过程和设备；"), None);
        // 数值被全角右括号闭合：`（见 4.4）进行策划、实施和控制：`行首 `4.4`
        // 吃成二级编号，旧版给 `###`，并把上一行「…对所需的过程（见」截断。
        assert_eq!(title_level("4.4）进行策划、实施和控制："), None);
        // 全角括号编号同样以标点收尾（GJB 目录残留「（5） （9）」曾被套 `##`）
        assert_eq!(title_level("（5） （9）"), None);
    }
}
