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
    let rest = rest.trim();
    match rest.chars().next() {
        Some(c) if c.is_alphanumeric() => {}
        _ => return None,
    }
    // N8 护栏：**标题不以句末标点收尾**（仅 ASCII 点分 / 裸中文数字两条路径）。
    //
    // GJB 9001C 文字版 38 页全量取证（`--text-only` 端到端，133 个标题行）：
    // 119 个真标题**无一**以句末标点结尾（末尾是汉字/罗马数字/页码，如
    // `4.4  质量管理体系及其过程`、`0.3.2  PDCA循环`）；13 个以标点结尾的**全部
    // 是误判**，且形态分两类：
    //
    // (a) **列表引导句**（12 条，主流形态）——国标条款正文以冒号引出枚举项：
    //     `#### 6.1.2  组织应策划：` 下一行就是 `a)  应对这些风险和机遇的措施；`。
    //     编号启发式把 `6.1.2` 吃成节号、剩下整句当标题，既误判又因「标题强制
    //     独段」把下面的 `a)` 枚举项与引导句之间的段落结构打断。
    // (b) **行尾截断残留**（1 条）——上行末尾「…中国电子科技集团公司第十」按行
    //     切断后行首剩「五研究所。」，裸中文数字 `五` 命中编号启发式。
    //
    // (a) 走 ASCII 点分路径但过去两道护栏都拦不住它（`组织` 首字是字母，不是
    // 标点），只有收尾标点能可靠区分；(b) 走裸中文数字路径。两条路径的收尾
    // 标点即误判信号。
    //
    // **中文括号路径（`（一）`）不在护栏范围内**——这是本护栏的刻意收窄：
    // 13 条误判里中文括号式**零命中**，GJB 全文 `（一）`/`（二）` 开头的行
    // **零命中**（`title_level` 在这份语料上从未从该路径产出过真标题）。按
    // 「未取证的形态不拦」（与 N1 护栏同原则：末尾带 `）` 的形态未在语料中
    // 出现，拦了是拿未取证的行为换收益），该路径保持原判。
    //
    // 这条收窄同时保住 `merge_into_paragraphs` 的一条既有回归：
    // `（一）理解组织及其环境。` + `组织应识别相关方。` 必须切成 2 段。
    // `（一）` 在本仓是**列表标记**（`list.rs::is_isolated_marker("（一）")`
    // 钉着，`marker_is_ordered` 文档注释明说「中文括号式 `（一）` MinerU
    // 不识别（kind=none）」），其段落边界原先正是靠 `title_level` 判标题
    // **顺带**给出的。护栏一旦覆盖该路径，那个顺带机制消失，两行被焊成 1 段。
    // 换言之：`（一）` 判成标题对「段落边界」这个下游效果是**必要**的，
    // 只是它的级别号（`Some(2)`）在无真实语料支撑前不宜被当作可信语义。
    //
    // 保守起见只拦句末/冒号/逗号类，**不拦** `、` 与 `）`：目次页形如
    // `- 5.3  组织的岗位、职责和权限……4` 内部就含顿号（但不在末尾），而末尾
    // 带 `）` 的形态未在语料中出现，拦了是拿未取证的行为换收益。
    //
    // `，` 同样纳入：GJB 取证命中 1 条（`8.2.3.1组织应确保有能力向顾客提供
    // 满足要求的产品和服务，在承诺向…`，长引导句被行宽截断在逗号处，下一行
    // 才是正文 `组织应对如下各项要求进行评审：`）。
    if !starts_with_cn_paren_numbering(t) {
        if let Some(last) = rest.chars().last()
            && matches!(
                last,
                '。' | '！' | '？' | '；' | '：' | '，' | '.' | '!' | '?' | ';' | ':' | ','
            )
        {
            return None;
        }
    }
    Some((dots + 2).clamp(2, 6))
}

/// 行首是**中文括号编号**（`（一）` / `（二）`）——N8 护栏的唯一豁免路径。
///
/// 与 [`parse_numbering`] 分支 2 的前半段同判据（全角开括号 + 纯中文数字
/// 内芯 + 可选全角右括号），单独抽成函数只为让 `title_level` 的护栏能区分
/// 「哪条编号路径来的」，不重复实现解析。
fn starts_with_cn_paren_numbering(t: &str) -> bool {
    let cs: Vec<char> = t.chars().collect();
    if cs.first() != Some(&'（') {
        return false;
    }
    let mut j = 1;
    let mut cnt = 0;
    while j < cs.len() && is_cn_numeral(cs[j]) {
        j += 1;
        cnt += 1;
    }
    cnt > 0 && (j == cs.len() || cs[j] == '）' || cs[j] == ')')
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
    //
    // N1护栏：**括号内纯 ASCII 数字的编号永不是标题**。中文技术文件里这形态是
    // 图内/表内引用标注，不是章条编号——GJB 9001C 第4 章的 PDCA 结构图里
    // 遍是 `（4）（5）（6）…`，图下注明确写「注：括号中的数字表示本标准的相应
    // 章节。」即这些数字**指向**章节而非开启章节。行内公式编号同源：段落被
    // 按行切断后行首剩 `(1) 式中…`，套上 `##` 再触发「标题强制独段」截断
    // 上一段，一个误判连带丢一整段。
    //
    // 只拦 ASCII 数字，不动中文数字：`一)` / `（一）` 是国标小节写法（见
    // `lines.rs::cn_numeral_paren_starts_own_paragraph`），且 `（一）范围`
    // 经GJB 全量回归确认为真标题。
    if cs[0] == '(' || cs[0] == '（' {
        let close = if cs[0] == '(' { ')' } else { '）' };
        let mut j = 1;
        let mut cnt = 0;
        while j < n && cs[j].is_ascii_digit() {
            j += 1;
            cnt += 1;
        }
        // 纯数字（无空白、无其他内容）才走到这；`（一）` 在上面的中文数字分支
        // 已经return，`（4a）` 这类混合形态 cnt 命中但闭合符不匹配，落到这里时
        // 括号内不是纯数字串 → 不当编号。
        if cnt > 0 && j < n && cs[j] == close {
            return None;
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
        // N1：`（1）` 这类括号内**纯 ASCII 数字**的编号不是标题（引用标注/行内
        // 公式编号），GJB 全量回归确认零标题——见`parenthesized_ascii_number_is_not_title`
        assert_eq!(title_level("（一）xx"), Some(2));
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

    /// N1 前置括号编号护栏的 GJB 9001C 真实取证。
    ///
    /// 真值来源：GJB 9001C―2017 第 4 章 PDCA 结构图（`--text-only` 端到端
    /// 产物第 166-176 行），图下注**明文**写「注：括号中的数字表示本标准的相应
    /// 章节。」——即 `(4)`/`（5）` 这些数字是**指向**章节的图内标注，不是章节
    /// 标题。护栏前该行被套成 `## （4） 支持（7），`，并因「标题强制独段」把
    /// 上行 `质量管理体系（4）` 顶成孤立块。
    ///
    /// 全文（38 页）另有`（5） （9）`（#16 护栏已拦）与「五研究所。」行尾
    /// 截断残留，**无一处** `(n)` 前置括号编号是真标题 → 护栏零新增。
    #[test]
    fn parenthesized_ascii_number_is_not_title() {
        // GJB 第 4 章 PDCA 图内标注（真实产物逐字）
        assert_eq!(title_level("（4） 支持（7），"), None);
        // 护栏前的真实误判形态（探针实测 Some(2)）
        assert_eq!(title_level("(1) 见图 3"), None);
        assert_eq!(title_level("（1）式中"), None);
        assert_eq!(title_level("(1) 过程"), None);
        // 纯编号行本来就该走None（回归）
        assert_eq!(title_level("(1)"), None);
        assert_eq!(title_level("（4）"), None);
        // 字母括号不是数字编号，一直就是 None（回归）
        assert_eq!(title_level("(a) 见附录"), None);
        // **中文数字括号是真标题**——护栏不得误伤（`lines.rs` 钉着
        // 「（一）是 title_level 标题，独段优先」）
        assert_eq!(title_level("（一）范围"), Some(2));
        assert_eq!(title_level("（二）术语"), Some(2));
    }

    /// N8 句末标点护栏的 GJB 9001C 全量取证。
    ///
    /// 数据基础：`--text-only` 端到端跑完 38 页，133 个标题行按末字符分类——
    /// **119 个真标题无一以句末标点结尾**（末尾是汉字/罗马数字/页码），**13 个
    /// 以标点结尾的全部是误判**。护栏前标题数 133，护栏后 119，**零新增**。
    #[test]
    fn sentence_punct_at_end_is_not_title() {
        // ── (a) 列表引导句（12 条，主流形态）─────────────────────────────
        // 国标条款以冒号引出枚举项，下一行就是 `a)  …`。过去两道护栏拦不住：
        // `组织应策划` 的首字是字母（非标点），只有收尾标点能可靠区分。
        assert_eq!(title_level("6.1.2  组织应策划："), None);
        assert_eq!(title_level("6.2.2  策划如何实现质量目标时，组织应确定："), None);
        assert_eq!(title_level("7.5.3.1  应控制质量管理体系和本标准所要求的成文信息，以确保："), None);
        assert_eq!(title_level("8.2.3.2  适用时，组织应保留与下列方面有关的成文信息："), None);
        assert_eq!(title_level("9.2.2  组织应："), None);
        assert_eq!(title_level("10.2.1  当出现不合格时，包括来自投诉的不合格，组织应："), None);
        // 句号收尾的长引导句（GJB line 725/306）
        assert_eq!(
            title_level("8.7.1  组织应确保对不符合要求的输出进行识别和控制，以防止非预期的使用或交付。"),
            None
        );
        // ── (b) 行尾截断残留（1 条）────────────────────────────────────
        // 上行末尾「…中国电子科技集团公司第十」按行切断后行首剩「五研究所。」，
        // 中文数字 `五` 命中编号启发式（旧版给 `##`）。
        assert_eq!(title_level("五研究所。"), None);
        // ── 真标题零回归：GJB 119 条真标题的代表形态 ────────────────────
        assert_eq!(title_level("1 范围"), Some(2));
        assert_eq!(title_level("0.3.2  PDCA循环"), Some(4));
        assert_eq!(title_level("4.4  质量管理体系及其过程"), Some(3));
        assert_eq!(title_level("5.1.1  总则"), Some(4));
        assert_eq!(title_level("7.1.5.1  总则"), Some(5));
        // 内部含顿号但末尾是汉字——不得误伤（`、` `，` 只看末字符）
        assert_eq!(title_level("5.3  组织的岗位、职责和权限"), Some(3));
        // 无编号关键词小节走前置 return，不经本护栏
        assert_eq!(title_level("ABSTRACT"), Some(2));
    }

    /// N8 护栏的**豁免路径**（中文括号式）——刻意收窄，如实钉住。
    ///
    /// 13 条标点收尾误判里中文括号式零命中，GJB 全文 `（一）` 开头的行零
    /// 命中，该路径既无真标题证据也无误判证据。且 `（一）` 判成标题对下游
    /// `merge_into_paragraphs` 的段落边界是**必要**的（见护栏注释末段）——
    /// 覆盖它会让 `（一）理解组织及其环境。` + `组织应识别相关方。` 焊成 1 段。
    #[test]
    fn cn_paren_numbering_is_exempt_from_punct_guard() {
        assert_eq!(title_level("（一）理解组织及其环境。"), Some(2));
        assert_eq!(title_level("（二）理解相关方。"), Some(2));
        // 裸中文数字路径**不豁免**——`五研究所。` 正是 (b) 类误判本体
        assert_eq!(title_level("五研究所。"), None);
        // 非括号的引导句照拦（对照 (a) 类）
        assert_eq!(title_level("6.1.2  组织应策划："), None);
    }

    /// N8 的**已知盲区**（暂不修，如实钉住）。
    ///
    /// GJB 另有引导句以汉字收尾、按行切分后末字不全，`6.1.1` 被套成标题：
    /// `#### 6.1.1  在策划质量管理体系时，组织应考虑到 4.1 所提及的因素和 4.2
    /// 所提及的要求，并确定…`，下一行是 `的风险和机遇，以：` + `a)` 枚举。
    ///
    /// **本护栏拦不住**（末字符是汉字不是标点）。要拦得判「引导句 + 后续
    /// `a)` 枚举」的**跨行结构**，那是列表识别层的活、不是单行 `title_level`
    /// 能定的——需要 `merge_into_paragraphs` 阶段的上下文，不能在标题判定里
    /// 凭单行猜。此处只把盲区写成断言，免得后来者以为已修完。
    #[test]
    fn known_blind_spot_long_guiding_sentence_split_by_line() {
        // 现状：仍然误判。修它需要跨行结构，不在 title_level 的能力边界内。
        assert_eq!(
            title_level("6.1.1  在策划质量管理体系时，组织应考虑到 4.1 所提及的因素和 4.2 所提及的要求，并确定"),
            Some(4)
        );
    }
}
