//! 行级后处理与段落合并。
//!
//! - 段落合并（`merge_into_paragraphs`）：块内相邻行按行距合并，对齐 MinerU
//!   `_merge_para_text`；
//! - 行级后处理（`postprocess_lines_boxed`）：西文连字符合并 + 全角 ASCII 归一化。

use super::list::starts_with_list_marker;
use super::title::title_level;

/// #11c-v3 字号护栏阈值：相邻行字号比值超过它 → 视为 block 边界开新段。
///
/// 阈值由 GJB 9001C 真实样本字号分布夹出（BACKLOG #11c-v3）：正文 10.0pt×1163
/// 行；标题 16/26pt（1.6×/2.6×，必触发）；**必须不触发**的干扰源——引用行
/// 11.3pt（1.13×）、页眉 10.5pt（1.05×）都低于它；**触发且语义正确**的最近
/// 对——注释 8.5pt（10/8.5=1.176×，注释块独立于正文）。1.15 恰落在 1.13 与
/// 1.176 之间。
pub(crate) const FONT_SIZE_GUARD_RATIO: f32 = 1.15;

/// 阅读序管线的**行载体**（#11b）：文本 + 几何 + 排序键 `y`。
///
/// 此前整条 OCR 阅读序管线的载体是裸 `String`（`order_structure` /
/// `postprocess_lines` / `body_regions` 全链路 `Vec<String>`），几何在
/// `order_within_block` 把 region 拍平成 `(y, text)` 时就丢了 → #11 的 content_list
/// v2 投影拿不到 bbox（实测 18 个 PDF 样本覆盖率 1/86）。本结构体把几何带上，
/// 管线的**合并点**（段落合并 / 连字符合并 / 列表项配对）统一取并集。
///
/// `bbox` 是 `Option`：**没有几何就是没有**，不填退化框（与 #6 第 1 步"不伪造
/// 分母"、#11 `Region::has_geometry` 同一条纪律）。#11b-v2 后文字层通路也全链
/// boxed，`Vec<String>` 薄封装已删。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Line {
    /// 排序键：行顶 y（段落合并按行距判定）。
    pub y: f32,
    pub text: String,
    /// `(x_min, x_max, y_min, y_max)`，与 [`crate::region::Region`] 同序同单位。
    pub bbox: Option<(f32, f32, f32, f32)>,
    /// 行字号（#11c-v3）：来自 [`crate::region::Region::font_size`]，段落合并
    /// 的字号护栏判据。`None` = 来源无字号证据（OCR det 框 / 无几何行）——
    /// 护栏对 None 不动作，OCR 通路零行为变化。
    pub font_size: Option<f32>,
}

impl Line {
    /// 从 region 造一行（几何 = 该 region 的框）。
    pub fn from_region(r: &crate::region::Region) -> Self {
        Self {
            y: r.y_min,
            text: r.text.clone(),
            bbox: if r.has_geometry() { Some((r.x_min, r.x_max, r.y_min, r.y_max)) } else { None },
            font_size: r.font_size,
        }
    }

    /// 只有文本的行（文字层通路 / 末级兜底）：**明确无几何**，不伪造。
    pub fn from_text(y: f32, text: impl Into<String>) -> Self {
        Self { y, text: text.into(), bbox: None, font_size: None }
    }

    /// 从 `Vec<String>` 造无几何行序列（薄封装用）。
    pub fn from_texts(texts: Vec<String>) -> Vec<Self> {
        texts.into_iter().map(|t| Self::from_text(0.0, t)).collect()
    }

    /// 并集（合并行时调用）：任一侧无几何 → 结果无几何（不拿一侧冒充整行）。
    pub fn union_bbox(a: Option<(f32, f32, f32, f32)>, b: Option<(f32, f32, f32, f32)>)
        -> Option<(f32, f32, f32, f32)> {
        match (a, b) {
            (Some(a), Some(b)) => {
                Some((a.0.min(b.0), a.1.max(b.1), a.2.min(b.2), a.3.max(b.3)))
            }
            // 一侧缺几何：另一侧也不能代表合并后的整行 → 记 None（宁缺勿造）
            _ => None,
        }
    }

    /// 字号并集（#11c-v3，合并行时调用）：双方 `Some` 取 max（护栏方向是
    /// "下行更大开新段"，合并段取段内最大字号才能拦住后续小字回吸）；
    /// 任一侧 `None` → `None`（同 [`Line::union_bbox`] 纪律：宁缺勿造）。
    pub fn union_font_size(a: Option<f32>, b: Option<f32>) -> Option<f32> {
        match (a, b) {
            (Some(a), Some(b)) => Some(a.max(b)),
            _ => None,
        }
    }
}

/// ADR-0009 D3：段落合并——相邻行 y 间距 < 行高 1.5x → 同段。
///
/// 对齐 MinerU `_merge_para_text`：行间无空行（间距小）则合并为一段。
/// 行高用块内中位 region 高度估计；空行/标题行不参与合并。
///
/// #11b：合并时几何取**并集**（合并后的段落横跨参与各行的纵向范围）。
///
/// #11c：消费方从 OCR fallback 扩展到文字层链路（`pdf/text_layer.rs` 尾步、
/// `ofd/mod.rs` PageData::Text）——文字层此前每视觉行即一段，长段落粒度
/// 丢失（GJB 真实样本实测长段 1 vs 扫描版 101），故提为 `pub(crate)` 跨模块复用。
///
/// 拼接统一按 MinerU `merge_para_with_text` 行语境规则（曾经按通路分档
/// `Concat`/`MineruLang`，OCR 侧独立 ticket 落地后三通路同档，枚举删除）：
/// **下行字母** CJK 占比 >= 0.5（或下行无字母时继承段落语境）→ 不加空格；
/// 西方语境补一个空格（英文换行词粘连 "anydoc-ocrText" 的修正）；
/// 行尾连字符不补空格（真连字符 e-Mail 类连着拼，可并连字符已在
/// postprocess 阶段合掉）。
///
/// #11c-v3 字号护栏：相邻行字号突变超过 [`FONT_SIZE_GUARD_RATIO`] → 双向开
/// 新段（模拟 MinerU 的 layout block 边界——4.0.8 文字层 span 模型不带字号、
/// 段落真值=版面框，本仓无版面模型，字号是无框条件下区分度最高的替代信号，
/// GJB 实测标题/正文 1.6×/2.6×）。单向会漏"大字号行被下一段小字从头部回吸"
/// （16pt「目 次」后跟 10pt 正文，`10 > 16×1.15` 不成立 → 并段），故双向。
/// 仅双方 `Some` 才判 → OCR/无字号源零行为变化。
pub(crate) fn merge_into_paragraphs(lines: &[Line]) -> Vec<Line> {
    if lines.is_empty() {
        return Vec::new();
    }
    // 行高估计：中位 region 高度（无 height 字段，用相邻行 y 差近似）
    let mut gaps: Vec<f32> = Vec::new();
    for w in lines.windows(2) {
        gaps.push((w[1].y - w[0].y).abs());
    }
    gaps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_gap = gaps.get(gaps.len() / 2).copied().unwrap_or(20.0).max(8.0);
    let merge_threshold = median_gap * 1.5;

    let mut out: Vec<Line> = Vec::new();
    let mut cur = lines[0].clone();
    for w in lines.windows(2) {
        let gap = (w[1].y - w[0].y).abs();
        // F3：标题检测需覆盖两类来路——编号启发式（`title_level`）在任何通路都
        // 可能命中；`#` 开头只可能是**来源文本自带的 markdown 字面量**（#6 第 2 步
        // 之后标题级别一律在装配**之后**才赋，故此处看不到 IR 级别；改造前三条
        // 通路同样如此，口径未变）。漏一条即会把标题并入正文段。
        let is_heading = |s: &str| title_level(s).is_some() || s.trim_start().starts_with('#');
        let next_is_heading = is_heading(&w[1].text);
        let cur_is_heading = is_heading(&w[0].text);
        // #10 切片 1：列表标记行开启**新段**（a)/（一）/- 等，数字式除外）；
        // 其后的续行（无标记）照常并入所在列表项——段首是标记、续行跟随。
        // 数字式维持不做：与标题编号冲突，title_level 已接住。
        let next_is_list = starts_with_list_marker(&w[1].text);
        // #10 切片 2：目次（INDEX）条目行逐条独立（点线引导形态，见
        // [`is_index_entry`]）——此前同字号 + 小间距 → 两两条目并成一段
        // （`前言…IV引言…V`），MinerU 是按 list 逐条输出的。
        let next_is_index = is_index_entry(&w[1].text);
        // #11c-v3：字号突变双向开新段（模拟 block 边界，见函数文档）。
        let font_break = match (cur.font_size, w[1].font_size) {
            (Some(c), Some(n)) => {
                n > c * FONT_SIZE_GUARD_RATIO || c > n * FONT_SIZE_GUARD_RATIO
            }
            _ => false,
        };
        // 标题行强制独段；新列表项开启新段；目次条目逐条；字号突变分段；
        // 间距超阈值则分段
        if cur_is_heading
            || next_is_heading
            || next_is_list
            || next_is_index
            || font_break
            || gap > merge_threshold
        {
            out.push(std::mem::replace(&mut cur, w[1].clone()));
        } else {
            // 同段：行间合并，拼接按下行语境补空格（MinerU 行语境规则）
            let next = w[1].clone();
            cur.bbox = Line::union_bbox(cur.bbox, next.bbox);
            cur.font_size = Line::union_font_size(cur.font_size, next.font_size);
            // #11c-v5：拼接 = docvortex `resolve_text_line_boundary`（边界两字符
            // 口径，取代旧的"整行语言检测 + 段落语境继承"）。
            let (head, sep) = resolve_line_boundary(&cur.text, &next.text);
            let next_text = next.text.trim_start();
            if next_text.is_empty() {
                continue;
            }
            cur.text = head;
            cur.text.push_str(sep);
            cur.text.push_str(next_text);
        }
    }
    out.push(cur);
    out
}

/// #10 切片 2：目次（INDEX）条目行——含连续 >= [`INDEX_DOTS_MIN`] 个点线引导
/// 字符（`…` `．` `.` `·` `・` `‥`）。
///
/// 动机：MinerU basic 把目录块按 `INDEX` 类型逐条输出（`- 前言……IV`）；本仓
/// 文字层无版面模型，目次条目同字号、间距小 → 被合并成两两一条
/// （`前言…IV引言…V`）。点线形态是文字层唯一能拿到的 INDEX 信号。
/// 阈值 3：正文省略号通常是 2 连（`……`），>=3 才是引导点线。
pub(crate) const INDEX_DOTS_MIN: usize = 3;

fn is_index_entry(line: &str) -> bool {
    let mut run = 0usize;
    for c in line.chars() {
        if matches!(c, '…' | '．' | '.' | '·' | '・' | '‥') {
            run += 1;
            if run >= INDEX_DOTS_MIN {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

// ── #11c-v5：行拼接 = docvortex `resolve_text_line_boundary` 逐条移植 ──
//
// 权威源：mineru 4.0.8 文字层 `content.py::_lines_to_block_content` →
// `docvortex.content.text.merge_text_line_contents` →
// `docvortex.foundation._text.resolve_text_line_boundary`（已实读源码移植）。
// 替换旧口径"下行字母 CJK 占比 >= 0.5 + 段落语境继承"：后者在真实语料上
// 有 11.9% 的边界判错（GJB 1069 对中 127 处分歧，其中 120 处是我们断词错误，
// 如 `…的公开网` + `站 ：www.iso.org` → 出 `公开网 站`）——根因是"整行语言
// 检测"猜的是段落语境，而 docvortex 只看**边界两侧各一个可见字符**。
//
// 与 Python 版的**已知差异**（均不影响实测语料，注释备查）：
// - 样式标记剥离（`_INLINE_STYLE_TAG_RE`）跳过：本仓 producer 不产 `<sup>`/
//   `**` 等标记（#6 决策 (c) 之后没有注入方），剥离是空操作；
// - Unicode 类别用显式集合近似（无 `unicode-general-category` 依赖）：Ps/Pi、
//   Pe/Pf、Mn/Cf 各按常见码点覆盖，未覆盖的码点退化为"补空格"分支；
// - `\d` 用 `is_ascii_digit`（Python `\d` 含全角数字，本仓全角已在
//   postprocess 归一化，等价）。

/// 行末断词连字符（docvortex `LINE_END_HYPHEN_CHARS`）。
const LINE_END_HYPHENS: [char; 5] = ['-', '\u{00AD}', '\u{2010}', '\u{2011}', '\u{2043}'];
/// 复合词前缀：行末连字符**不删**（docvortex `_COMPOUND_PREFIXES`）。
const COMPOUND_PREFIXES: [&str; 11] = [
    "open", "non", "self", "cross", "anti", "pre", "post", "co", "semi", "multi", "quasi",
];
/// 复合词连接词：行末连字符**不删**（docvortex `_COMPOUND_CONNECTORS`）。
const COMPOUND_CONNECTORS: [&str; 9] = ["of", "the", "to", "and", "in", "for", "on", "by", "with"];
/// 中日标点（docvortex `_CJK_PUNCTUATION`）。
const CJK_PUNCTUATION: [char; 22] = [
    '，', '。', '！', '？', '；', '：', '、', '（', '）', '【', '】', '《', '》', '〈', '〉', '「',
    '」', '『', '』', '〔', '］', '｛',
];
/// 闭标点（docvortex `_CLOSING_PUNCTUATION`）——出现在下行开头则直连。
const CLOSING_PUNCTUATION: [char; 7] = [',', '.', ';', ':', '!', '?', '%'];

/// docvortex `resolve_text_line_boundary`：返回 `(处理后的上行, 边界分隔符)`。
/// 分隔符 `" "` 或 `""`；上行可能被删掉一个行末断词连字符。
pub(crate) fn resolve_line_boundary(prev: &str, next: &str) -> (String, &'static str) {
    let processed = prev.trim_end();
    if processed.is_empty() {
        return (String::new(), "");
    }
    let stripped_next = next.trim_start();
    if stripped_next.is_empty() {
        return (processed.to_string(), "");
    }
    // 1) 严格横跨边界的 URL 直连；下行自身就是完整 URL 开头 → 保留空格
    if url_spans_boundary(processed, stripped_next) {
        let sep = if starts_with_url(stripped_next) { " " } else { "" };
        return (processed.to_string(), sep);
    }
    // 2) 行末英文断词符（`[A-Za-z]+[hyphen]$`）
    if let Some(word) = line_end_hyphen_word(processed) {
        // 缩写（GLGE-difficult）与固定复合词的连字符有语义，不能当断词符删
        let acronym = word.len() > 1 && word.chars().all(|c| c.is_ascii_uppercase());
        let lower = word.to_ascii_lowercase();
        let hard = processed.ends_with('-')
            && (COMPOUND_PREFIXES.contains(&lower.as_str())
                || (COMPOUND_CONNECTORS.contains(&lower.as_str())
                    && processed[..processed.len() - word.len() - 1].ends_with('-')));
        let next_lower = stripped_next.chars().next().is_some_and(|c| c.is_lowercase());
        if next_lower && !acronym && !hard {
            // 删断词符后直连："mainten-" + "ance" → "maintenance"
            let cut = processed.len() - processed.chars().next_back().unwrap().len_utf8();
            return (processed[..cut].to_string(), "");
        }
        return (processed.to_string(), "");
    }
    // 3) `数字[-–]$` + 下行数字 → 直连（页码/编号跨行）
    if ends_with_digit_dash(processed)
        && stripped_next.chars().next().is_some_and(|c| c.is_ascii_digit())
    {
        return (processed.to_string(), "");
    }
    // 3b) **唯一一处有实证的私有偏离**：两侧边界都是 ASCII 数字 → 直连。
    // docvortex 在这里补空格（它假设输入是段落文本行），但本仓文字层还承担
    // "表格窄列把一串数字挤成多个视觉行"的还原（GJB 表 C.1：`1`/`3` 两行 →
    // 严格对齐会出 `1 3`；`34.4.1` 被拆成 `3 4.4.1` 后连标题级别都从 4 级降
    // 到 2 级——GJB 实测 6 处结构性退化）。代价：正文里两个**独立**数字块
    // 跨行相邻（英文 `in 2024` / `2025 will`）会被误连，实测语料未出现。
    // 表格结构票（#9）落地后，单元格内容不再走行拼接，本条应随之删除。
    if processed.chars().next_back().is_some_and(|c| c.is_ascii_digit())
        && stripped_next.chars().next().is_some_and(|c| c.is_ascii_digit())
    {
        return (processed.to_string(), "");
    }
    // 4) 边界两侧各取一个可见字符判定
    let (Some(pc), Some(nc)) = (
        boundary_char(processed, true),
        boundary_char(stripped_next, false),
    ) else {
        return (processed.to_string(), "");
    };
    if is_unspaced_char(pc) || is_unspaced_char(nc) {
        return (processed.to_string(), "");
    }
    if is_opening_char(pc) {
        return (processed.to_string(), "");
    }
    if is_closing_char(nc) || CLOSING_PUNCTUATION.contains(&nc) {
        return (processed.to_string(), "");
    }
    (processed.to_string(), " ")
}

/// docvortex `_is_unspaced_character`：汉字、假名及中日标点（**谚文不算**——
/// 韩文按西文留词间空格）。
fn is_unspaced_char(c: char) -> bool {
    CJK_PUNCTUATION.contains(&c)
        || matches!(c,
            '\u{3000}'..='\u{303F}'   // CJK 符号与标点
            | '\u{3040}'..='\u{30FF}' // 平假名/片假名
            | '\u{31F0}'..='\u{31FF}' // 片假名语音扩展
            | '\u{3400}'..='\u{4DBF}' // 扩展 A
            | '\u{4E00}'..='\u{9FFF}' // CJK 统一表意文字
            | '\u{F900}'..='\u{FAFF}' // 兼容表意文字
            | '\u{FF61}'..='\u{FF9F}' // 半角片假名
            | '\u{1AFF0}'..='\u{1AFFF}'
            | '\u{1B000}'..='\u{1B16F}'
            | '\u{20000}'..='\u{2EE5F}'
            | '\u{2F800}'..='\u{2FA1F}'
            | '\u{30000}'..='\u{323AF}')
}

/// Unicode 类别 Ps/Pi（开括号/前导引号）的常用码点近似。
fn is_opening_char(c: char) -> bool {
    matches!(c,
        '(' | '[' | '{' | '\u{00AB}' | '\u{2018}' | '\u{201B}' | '\u{201C}' | '\u{201F}'
        | '\u{2039}' | '「' | '『' | '（' | '【' | '〔' | '［' | '｛' | '《' | '〈' | '〝' | '﹁' | '﹃')
}

/// Unicode 类别 Pe/Pf（闭括号/后随引号）的常用码点近似。
fn is_closing_char(c: char) -> bool {
    matches!(c,
        ')' | ']' | '}' | '\u{00BB}' | '\u{2019}' | '\u{201D}' | '\u{203A}' | '」' | '』'
        | '）' | '】' | '〕' | '］' | '｝' | '》' | '〉' | '〞' | '﹂' | '﹄')
}

/// docvortex `_boundary_character`：取边界上第一个"可见"字符，跳过组合标记
/// （M*）与格式/控制/私用区（C*）。
fn boundary_char(s: &str, from_end: bool) -> Option<char> {
    let mut iter: Box<dyn Iterator<Item = char>> = if from_end {
        Box::new(s.chars().rev())
    } else {
        Box::new(s.chars())
    };
    iter.find(|&c| {
        !(c.is_control()
            || matches!(c,
                // Mn/Me/Mc：组合附加符号与变体选择符
                '\u{0300}'..='\u{036F}' | '\u{1AB0}'..='\u{1AFF}' | '\u{20D0}'..='\u{20FF}'
                | '\u{FE00}'..='\u{FE0F}' | '\u{FE20}'..='\u{FE2F}'
                // Cf/Cc/Cs/Co：软连字符、零宽、BOM、私用区
                | '\u{00AD}' | '\u{200B}'..='\u{200F}' | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}' | '\u{FEFF}' | '\u{E000}'..='\u{F8FF}'
                | '\u{F0000}'..='\u{FFFFD}' | '\u{100000}'..='\u{10FFFD}')
            )
    })
}

/// 行末断词符前的英文单词（`[A-Za-z]+[hyphen]$`）。
fn line_end_hyphen_word(processed: &str) -> Option<String> {
    let last = processed.chars().next_back()?;
    if !LINE_END_HYPHENS.contains(&last) {
        return None;
    }
    let head = &processed[..processed.len() - last.len_utf8()];
    let word: String = head
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    if word.is_empty() {
        None
    } else {
        Some(word)
    }
}

/// `\d[-–]$`（数字后紧跟连字符/en dash 结尾）。
fn ends_with_digit_dash(s: &str) -> bool {
    let mut it = s.chars().rev();
    matches!(it.next(), Some('-' | '\u{2013}'))
        && it.next().is_some_and(|c| c.is_ascii_digit())
}

fn is_url_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '~' | ':' | '/' | '?' | '#' | '[' | ']' | '@' | '!' | '$' | '&' | '\'' | '(' | ')' | '*' | '+' | ',' | ';' | '=' | '%' | '-')
}

/// 下行自身是不是以完整 URL 开头（`_URL_AT_LINE_START_RE`）。
fn starts_with_url(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("ftp://")
        || lower.starts_with("www.")
}

/// docvortex `_url_spans_line_boundary`：拼接串里存在**严格跨边界**的 URL 候选。
fn url_spans_boundary(prev: &str, next: &str) -> bool {
    let boundary = prev.chars().count();
    let mut cand = String::with_capacity(prev.len() + next.len());
    cand.push_str(prev);
    cand.push_str(next);
    let cs: Vec<char> = cand.chars().collect();
    let mut i = 0usize;
    while i < cs.len() {
        if url_starts_at(&cs, i) && (i == 0 || !cs[i - 1].is_ascii_alphanumeric()) {
            let mut j = i;
            while j < cs.len() && is_url_char(cs[j]) {
                j += 1;
            }
            if i < boundary && boundary < j {
                return true;
            }
            i = j;
        } else {
            i += 1;
        }
    }
    false
}

/// `cs[i..]` 是否以 URL scheme / `www.` 开头（ASCII 大小写不敏感）。
fn url_starts_at(cs: &[char], i: usize) -> bool {
    let tail: String = cs[i..].iter().take(11).collect();
    let lower = tail.to_ascii_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("ftp://")
        || lower.starts_with("www.")
}

/// 行级后处理：西文连字符合并 + 全角 ASCII 归一化。
///
/// 借鉴 MinerU `merge_para_with_text`/`full_to_half_exclude_marks`：
/// - 行尾 ASCII 连字符 + 下行以小写字母开头 → 合并断词（如 "mainten-" + "ance" → "maintenance"）。
/// - 全角数字/字母 → 半角（０-９→0-9，Ａ-Ｚ→A-Z，ａ-ｚ→a-z）；中文全角标点保留。
///
/// 行级后处理（**带几何**，#11b 真相函数）：连字符合并取并集 + 全角归一化。
/// #11b-v2 后 `Vec<String>` 薄封装已删（文字层通路全链 boxed，真相只有一份）。
pub(crate) fn postprocess_lines_boxed(lines: Vec<Line>) -> Vec<Line> {
    merge_hyphenated_lines_boxed(lines)
        .into_iter()
        .map(|mut l| {
            l.text = normalize_full_width_ascii(&l.text);
            l
        })
        .collect()
}

/// 西文连字符合并：行尾 `-` 且下一行以小写字母开头时，去连字符拼接（无空格）。
///
/// #11b：合并两行 → 几何取**并集**（断词被拼回一行，框应覆盖两行）。
fn merge_hyphenated_lines_boxed(lines: Vec<Line>) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::with_capacity(lines.len());
    let mut iter = lines.into_iter().peekable();
    while let Some(mut cur) = iter.next() {
        loop {
            let Some(next) = iter.peek() else { break };
            let cur_trim = cur.text.trim_end();
            let Some(base) = cur_trim.strip_suffix('-') else {
                break;
            };
            let nxt = next.text.trim_start();
            let Some(c) = nxt.chars().next() else { break };
            if !c.is_ascii_lowercase() {
                break;
            }
            let base = base.to_string();
            let nxt = nxt.to_string();
            let next_bbox = iter.next().expect("peeked").bbox;
            cur.bbox = Line::union_bbox(cur.bbox, next_bbox);
            cur.text = format!("{base}{nxt}");
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
    use super::{Line, merge_into_paragraphs, postprocess_lines_boxed, resolve_line_boundary};

    /// 测试视图：String 进 String 出（生产薄封装已删，仅测试沿用旧断言写法）。
    fn postprocess_lines(lines: Vec<String>) -> Vec<String> {
        postprocess_lines_boxed(Line::from_texts(lines))
            .into_iter()
            .map(|l| l.text)
            .collect()
    }

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
            Line { y: 100.0, text: "第一行".into(), bbox: Some((10.0, 90.0, 100.0, 110.0)), font_size: None },
            Line { y: 110.0, text: "第二行".into(), bbox: Some((10.0, 90.0, 110.0, 120.0)), font_size: None },
            Line { y: 120.0, text: "第三行".into(), bbox: Some((10.0, 90.0, 120.0, 130.0)), font_size: None },
            Line { y: 200.0, text: "第二段".into(), bbox: Some((10.0, 90.0, 200.0, 210.0)), font_size: None },
        ];
        let out = merge_into_paragraphs(&lines);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].text, "第一行第二行第三行");
        assert_eq!(out[1].text, "第二段");
        // #11b：合并段的几何 = 参与各行的**并集**（纵向被拉通，横向不变）
        assert_eq!(out[0].bbox, Some((10.0, 90.0, 100.0, 130.0)));
    }

    /// ADR-0009 D3：标题行（# 开头）强制独段，不与相邻行合并。
    #[test]
    fn merge_into_paragraphs_heading_standalone() {
        let lines = vec![
            Line { y: 100.0, text: "# 标题".into(), bbox: None, font_size: None },
            Line { y: 110.0, text: "正文".into(), bbox: None, font_size: None },
        ];
        let out = merge_into_paragraphs(&lines);
        let texts: Vec<&str> = out.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, vec!["# 标题", "正文"]);
    }

    /// #11c：拼接按 MinerU 行语境规则——下行**字母** CJK 语境不补空格（数字/
    /// 标点不计，`第二段：发票号码 2024001，金额 1280.00 元。` 是中文语境）；
    /// 西方语境补空格（text.pdf 实测词粘连 "anydoc-ocrText" 的修正）；
    /// 行尾连字符不补空格（真连字符 e-Mail 类连着拼）。
    /// 三通路同档（OCR 侧 `Concat` 分档在 #11c-v2 落地后删除）。
    #[test]
    fn merge_into_paragraphs_space_by_line_language() {
        // 英文行合并 → 行间补空格
        let en = vec![
            Line { y: 100.0, text: "Hello anydoc-ocr".into(), bbox: None, font_size: None },
            Line { y: 110.0, text: "Text PDF smoke test".into(), bbox: None, font_size: None },
        ];
        assert_eq!(
            merge_into_paragraphs(&en)[0].text,
            "Hello anydoc-ocr Text PDF smoke test"
        );
        // 中文行合并 → 不补空格
        let zh = vec![
            Line { y: 100.0, text: "质量管理".into(), bbox: None, font_size: None },
            Line { y: 110.0, text: "体系要求".into(), bbox: None, font_size: None },
        ];
        assert_eq!(merge_into_paragraphs(&zh)[0].text, "质量管理体系要求");
        // 中文行夹大量数字仍是中文语境（字母口径，数字不计）→ 不补
        let zh_num = vec![
            Line { y: 100.0, text: "上句结束。".into(), bbox: None, font_size: None },
            Line { y: 110.0, text: "第二段：发票号码 2024001，金额 1280.00 元。".into(), bbox: None, font_size: None },
        ];
        assert_eq!(
            merge_into_paragraphs(&zh_num)[0].text,
            "上句结束。第二段：发票号码 2024001，金额 1280.00 元。"
        );
        // 西方语境 + 行尾连字符 → 不补空格（真连字符 e-Mail 类连着拼）
        let hy = vec![
            Line { y: 100.0, text: "well-".into(), bbox: None, font_size: None },
            Line { y: 110.0, text: "Known".into(), bbox: None, font_size: None },
        ];
        assert_eq!(merge_into_paragraphs(&hy)[0].text, "well-Known");
    }

    /// #10 切片 1：列表标记行开启**新段**、续行（无标记）并入所在列表项；
    /// 正文段互并行为不变。
    #[test]
    fn merge_into_paragraphs_list_items_start_own_paragraph() {
        let lines = vec![
            Line { y: 100.0, text: "编制产品标准化大纲；".into(), bbox: None, font_size: None },
            Line { y: 110.0, text: "f)  确定产品通用化、系列化要求，".into(), bbox: None, font_size: None },
            Line { y: 120.0, text: "覆盖接口与互换性。".into(), bbox: None, font_size: None },
            Line { y: 130.0, text: "g)  按照 GJB 450 的要求确定工作项目".into(), bbox: None, font_size: None },
        ];
        let out = merge_into_paragraphs(&lines);
        assert_eq!(out.len(), 3, "正文段 + f) 项（含续行）+ g) 项: {out:?}");
        assert_eq!(
            out[1].text,
            "f)  确定产品通用化、系列化要求，覆盖接口与互换性。",
            "f) 的续行并入其段"
        );
        // bullet 行开启新段、续行并入；（一）行由 title_level 判中文编号标题，
        // is_heading 护栏先命中独段——list 护栏对它冗余不冲突
        let zh = vec![
            Line { y: 100.0, text: "- 要点一：组织应识别相关方。".into(), bbox: None, font_size: None },
            Line { y: 110.0, text: "相关方包括顾客、供方与监管机构。".into(), bbox: None, font_size: None },
            Line { y: 120.0, text: "- 要点二：组织应开展相关方分析。".into(), bbox: None, font_size: None },
        ];
        let out = merge_into_paragraphs(&zh);
        assert_eq!(out.len(), 2, "两个 bullet 项各自成段: {out:?}");
        assert_eq!(out[0].text, "- 要点一：组织应识别相关方。相关方包括顾客、供方与监管机构。");
        let cn = vec![
            Line { y: 100.0, text: "（一）理解组织及其环境。".into(), bbox: None, font_size: None },
            Line { y: 110.0, text: "组织应识别相关方。".into(), bbox: None, font_size: None },
        ];
        assert_eq!(merge_into_paragraphs(&cn).len(), 2, "（一）是 title_level 标题，独段优先");
        // 无标记正文行照旧合并
        let plain = vec![
            Line { y: 100.0, text: "第一段。".into(), bbox: None, font_size: None },
            Line { y: 110.0, text: "第二行内容。".into(), bbox: None, font_size: None },
        ];
        assert_eq!(merge_into_paragraphs(&plain)[0].text, "第一段。第二行内容。");
    }

    /// #11b：连字符合并（两行拼回一行）→ 几何取并集，不是取第一行。
    #[test]
    fn postprocess_boxed_merges_hyphen_and_unions_boxes() {
        let lines = vec![
            Line { y: 100.0, text: "mainten-".into(), bbox: Some((10.0, 90.0, 100.0, 110.0)), font_size: None },
            Line { y: 120.0, text: "ance".into(), bbox: Some((10.0, 80.0, 120.0, 130.0)), font_size: None },
        ];
        let out = postprocess_lines_boxed(lines);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "maintenance");
        assert_eq!(out[0].bbox, Some((10.0, 90.0, 100.0, 130.0)));
    }

    /// #11b：一侧无几何 → 合并结果**无几何**（不拿半边冒充整行）。
    #[test]
    fn union_bbox_is_none_when_either_side_lacks_geometry() {
        let a = Some((10.0, 90.0, 100.0, 110.0));
        assert_eq!(Line::union_bbox(a, None), None);
        assert_eq!(Line::union_bbox(None, a), None);
        assert_eq!(Line::union_bbox(None, None), None);
    }

    /// #11b：无几何的行经后处理仍是原文本（文字层通路薄封装的行为不变）。
    #[test]
    fn postprocess_plain_keeps_text_when_no_geometry() {
        assert_eq!(
            postprocess_lines(vec!["mainten-".into(), "ance".into()]),
            vec!["maintenance"]
        );
    }

    /// #11c-v3 辅助：造带字号的行（间距 10，远小于 merge 阈值，纯测字号护栏）。
    fn fs(y: f32, size: f32, text: &str) -> Line {
        Line {
            y,
            text: text.into(),
            bbox: None,
            font_size: Some(size),
        }
    }

    /// 字号突变双向开新段：正文后跟大字号标题不并入（下行更大），
    /// 大字号行后的小字也不被从头部回吸（上行更大）。
    #[test]
    fn font_size_break_opens_paragraph_both_directions() {
        // 10pt 正文 → 16pt「目 次」：next 更大 → 标题独立成段
        let lines = vec![fs(100.0, 10.0, "正文第一段。"), fs(110.0, 16.0, "目    次")];
        let out = merge_into_paragraphs(&lines);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].text, "目    次");
        // 16pt 标题 → 10pt 正文：cur 更大 → 正文不得回吸标题（单向护栏的漏洞）
        let lines = vec![fs(100.0, 16.0, "前   言"), fs(110.0, 10.0, "本标准规定了。")];
        let out = merge_into_paragraphs(&lines);
        assert_eq!(out.len(), 2, "大字号行后的小字不得把标题从头部吞回");
        assert_eq!(out[0].text, "前   言");
    }

    /// 同字号照常合并；合并段字号 = 段内 max（后续护栏用段值比较）。
    #[test]
    fn same_font_size_still_merges_and_unions_to_max() {
        let lines = vec![fs(100.0, 10.0, "第一行"), fs(110.0, 10.0, "第二行")];
        let out = merge_into_paragraphs(&lines);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "第一行第二行");
        assert_eq!(out[0].font_size, Some(10.0));
    }

    /// 字号证据缺一侧（None）→ 护栏不动作：OCR 通路与无字号源零行为变化。
    #[test]
    fn missing_font_size_disables_guard() {
        let lines = vec![
            Line { y: 100.0, text: "OCR行一".into(), bbox: None, font_size: None },
            Line { y: 110.0, text: "OCR行二".into(), bbox: None, font_size: None },
        ];
        let out = merge_into_paragraphs(&lines);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].font_size, None);
        // 一侧 Some 一侧 None（混合来源）：护栏不动作，字号并集记 None（宁缺勿造）
        let lines = vec![fs(100.0, 10.0, "有字号行"), Line { y: 110.0, text: "无字号行".into(), bbox: None, font_size: None }];
        let out = merge_into_paragraphs(&lines);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].font_size, None);
    }

    /// #10 切片 2：目次条目（>=3 连点线）逐条独立；正文 2 连省略号照旧合并。
    #[test]
    fn index_entries_stay_separate_lines() {
        let lines = vec![
            fs(100.0, 10.0, "前言……………………………………………………………………………………IV"),
            fs(110.0, 10.0, "引言………………………………………………………………………………………V"),
        ];
        let out = merge_into_paragraphs(&lines);
        assert_eq!(out.len(), 2, "目次两条目不得并成一段");
        // 正文里的 2 连省略号（……）不是引导点线 → 照旧合并
        let lines = vec![fs(100.0, 10.0, "他说……"), fs(110.0, 10.0, "后来就没了。")];
        assert_eq!(merge_into_paragraphs(&lines).len(), 1);
    }

    /// #11c-v5：真值表由 **MinerU 4.0.8 的 `docvortex.foundation._text
    /// .resolve_text_line_boundary` 实跑生成**（一次性脚本，非手算），覆盖
    /// URL 跨行 / 断词连字符 / 缩写 / 复合词 / 数字连字符 / Ps-Pi / Pe-Pf
    /// / 闭标点 / 汉字 / 假名 / **谚文留空格** / 空行 全部分支。
    #[test]
    fn line_boundary_matches_docvortex_truth_table() {
        // (上行, 下行, 期望处理后的上行, 期望分隔符)
        let cases: Vec<(&str, &str, &str, &str)> = vec![
            ("质量管理", "体系要求", "质量管理", ""),
            ("Hello anydoc-ocr", "Text PDF", "Hello anydoc-ocr", " "),
            ("上句结束。", "第二段：发票号码 2024001", "上句结束。", ""),
            // 旧口径在这里会断错词（`公开网 站`）：docvortex 只看边界字符
            ("the public web", "site : www.iso.org", "the public web", " "),
            ("的公开网", "站 ：www.iso.org", "的公开网", ""),
            ("www.iso.org/stand", "ard.html", "www.iso.org/stand", ""),
            ("visit www.a.com/", "www.b.com", "visit www.a.com/", " "),
            ("https://a.com/b", "c.html", "https://a.com/b", ""),
            // 断词连字符：仅下一行小写才删；缩写/复合词/大写开头不删
            ("mainten-", "ance", "mainten", ""),
            ("e-", "mail", "e", ""),
            ("well-", "Known", "well-", ""),
            ("GLGE-", "difficult", "GLGE-", ""),
            ("non-", "profit", "non-", ""),
            ("state-of-the-", "art", "state-of-the-", ""),
            // 数字 + 连字符/en dash
            ("2024-", "001", "2024-", ""),
            ("2017\u{2013}", "05", "2017\u{2013}", ""),
            // 私有豁免（唯一偏离 docvortex）：边界两侧都是数字 → 直连
            // （docvortex 真值为 " "，本仓为还原表格窄列断行而豁免，见函数注释）
            ("编号 12", "345 结束", "编号 12", ""),
            // Ps/Pi 与 Pe/Pf、闭标点
            ("open (", "see)", "open (", ""),
            ("hello", ", world", "hello", ""),
            ("hello", "world.", "hello", " "),
            // 谚文**不是** unspaced（docvortex 明确：韩文按西文留空格）
            ("안녕", "하세요", "안녕", " "),
            ("（一）", "范围", "（一）", ""),
            ("", "x", "", ""),
            ("abc", "   ", "abc", ""),
            ("PDCA 循环如下：", "——策划（Plan）", "PDCA 循环如下：", ""),
            ("质量管理体系要求", "Quality management", "质量管理体系要求", ""),
        ];
        for (prev, next, want_head, want_sep) in cases {
            let (head, sep) = resolve_line_boundary(prev, next);
            assert_eq!(
                (head.as_str(), sep),
                (want_head, want_sep),
                "prev={prev:?} next={next:?}"
            );
        }
    }

    /// 阈值下方不触发：页眉 10.5pt（1.05×）与引用行 11.3pt（1.13×）随正文合并。
    #[test]
    fn subthreshold_ratio_still_merges() {
        let lines = vec![fs(100.0, 10.0, "正文行。"), fs(110.0, 11.3, "GB/T 19000 引用行")];
        let out = merge_into_paragraphs(&lines);
        assert_eq!(out.len(), 1, "1.13 < 1.15 应合并（GJB 实测干扰源）");
    }
}


