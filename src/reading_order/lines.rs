//! 行级后处理与段落合并。
//!
//! - 段落合并（`merge_into_paragraphs`）：块内相邻行按行距合并，对齐 MinerU
//!   `_merge_para_text`；
//! - 行级后处理（`postprocess_lines_boxed`）：西文连字符合并 + 全角 ASCII 归一化。

use super::list::starts_with_list_marker;
use super::title::title_level;

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
}

impl Line {
    /// 从 region 造一行（几何 = 该 region 的框）。
    pub fn from_region(r: &crate::region::Region) -> Self {
        Self {
            y: r.y_min,
            text: r.text.clone(),
            bbox: if r.has_geometry() { Some((r.x_min, r.x_max, r.y_min, r.y_max)) } else { None },
        }
    }

    /// 只有文本的行（文字层通路 / 末级兜底）：**明确无几何**，不伪造。
    pub fn from_text(y: f32, text: impl Into<String>) -> Self {
        Self { y, text: text.into(), bbox: None }
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
        // 标题行强制独段；新列表项开启新段；间距超阈值则分段
        if cur_is_heading || next_is_heading || next_is_list || gap > merge_threshold {
            out.push(std::mem::replace(&mut cur, w[1].clone()));
        } else {
            // 同段：行间合并，拼接按下行语境补空格（MinerU 行语境规则）
            let next = w[1].clone();
            cur.bbox = Line::union_bbox(cur.bbox, next.bbox);
            let next_text = next.text.trim_start();
            if next_text.is_empty() {
                continue;
            }
            // 下行无字母（纯数字/标点）→ 继承段落已积累语境；两者皆无
            // 字母 → 视为中文语境不加空格（数字行多见于中文票据语境）。
            let zh = cjk_dominant_letters(&next.text)
                .or_else(|| cjk_dominant_letters(&cur.text))
                .unwrap_or(true);
            if zh || cur.text.ends_with('-') {
                // 行尾连字符不补空格：postprocess 已先行合并可并连字符，
                // 能留到这里的都是真连字符（e-Mail 类），连着拼。
                cur.text.push_str(next_text);
            } else {
                if !cur.text.ends_with(' ') {
                    cur.text.push(' ');
                }
                cur.text.push_str(next_text);
            }
        }
    }
    out.push(cur);
    out
}

/// MinerU `detect_lang` 的字母口径移植：只统计**字母**（`is_alphabetic`，数字/
/// 标点不计——`第二段：发票号码 2024001，金额 1280.00 元。` 是中文语境而非西方）。
/// 字母中 CJK（汉字/假名/谚文）占比 >= 0.5 → `Some(true)`（中文语境）；
/// 否则 `Some(false)`（西方语境）；无字母 → `None`（由调用方继承段落语境）。
fn cjk_dominant_letters(s: &str) -> Option<bool> {
    let mut letters = 0usize;
    let mut cjk = 0usize;
    for c in s.chars() {
        if c.is_alphabetic() {
            letters += 1;
            if matches!(c,
                '\u{4E00}'..='\u{9FFF}'   // CJK 统一表意文字
                | '\u{3400}'..='\u{4DBF}' // 扩展 A
                | '\u{3040}'..='\u{30FF}' // 平假名/片假名
                | '\u{AC00}'..='\u{D7AF}' // 谚文音节
                | '\u{F900}'..='\u{FAFF}' // 兼容表意文字
            ) {
                cjk += 1;
            }
        }
    }
    if letters == 0 {
        return None;
    }
    Some(cjk * 2 >= letters)
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
    use super::{Line, merge_into_paragraphs, postprocess_lines_boxed};

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
            Line { y: 100.0, text: "第一行".into(), bbox: Some((10.0, 90.0, 100.0, 110.0)) },
            Line { y: 110.0, text: "第二行".into(), bbox: Some((10.0, 90.0, 110.0, 120.0)) },
            Line { y: 120.0, text: "第三行".into(), bbox: Some((10.0, 90.0, 120.0, 130.0)) },
            Line { y: 200.0, text: "第二段".into(), bbox: Some((10.0, 90.0, 200.0, 210.0)) },
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
            Line { y: 100.0, text: "# 标题".into(), bbox: None },
            Line { y: 110.0, text: "正文".into(), bbox: None },
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
            Line { y: 100.0, text: "Hello anydoc-ocr".into(), bbox: None },
            Line { y: 110.0, text: "Text PDF smoke test".into(), bbox: None },
        ];
        assert_eq!(
            merge_into_paragraphs(&en)[0].text,
            "Hello anydoc-ocr Text PDF smoke test"
        );
        // 中文行合并 → 不补空格
        let zh = vec![
            Line { y: 100.0, text: "质量管理".into(), bbox: None },
            Line { y: 110.0, text: "体系要求".into(), bbox: None },
        ];
        assert_eq!(merge_into_paragraphs(&zh)[0].text, "质量管理体系要求");
        // 中文行夹大量数字仍是中文语境（字母口径，数字不计）→ 不补
        let zh_num = vec![
            Line { y: 100.0, text: "上句结束。".into(), bbox: None },
            Line { y: 110.0, text: "第二段：发票号码 2024001，金额 1280.00 元。".into(), bbox: None },
        ];
        assert_eq!(
            merge_into_paragraphs(&zh_num)[0].text,
            "上句结束。第二段：发票号码 2024001，金额 1280.00 元。"
        );
        // 西方语境 + 行尾连字符 → 不补空格（真连字符 e-Mail 类连着拼）
        let hy = vec![
            Line { y: 100.0, text: "well-".into(), bbox: None },
            Line { y: 110.0, text: "Known".into(), bbox: None },
        ];
        assert_eq!(merge_into_paragraphs(&hy)[0].text, "well-Known");
    }

    /// #10 切片 1：列表标记行开启**新段**、续行（无标记）并入所在列表项；
    /// 正文段互并行为不变。
    #[test]
    fn merge_into_paragraphs_list_items_start_own_paragraph() {
        let lines = vec![
            Line { y: 100.0, text: "编制产品标准化大纲；".into(), bbox: None },
            Line { y: 110.0, text: "f)  确定产品通用化、系列化要求，".into(), bbox: None },
            Line { y: 120.0, text: "覆盖接口与互换性。".into(), bbox: None },
            Line { y: 130.0, text: "g)  按照 GJB 450 的要求确定工作项目".into(), bbox: None },
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
            Line { y: 100.0, text: "- 要点一：组织应识别相关方。".into(), bbox: None },
            Line { y: 110.0, text: "相关方包括顾客、供方与监管机构。".into(), bbox: None },
            Line { y: 120.0, text: "- 要点二：组织应开展相关方分析。".into(), bbox: None },
        ];
        let out = merge_into_paragraphs(&zh);
        assert_eq!(out.len(), 2, "两个 bullet 项各自成段: {out:?}");
        assert_eq!(out[0].text, "- 要点一：组织应识别相关方。相关方包括顾客、供方与监管机构。");
        let cn = vec![
            Line { y: 100.0, text: "（一）理解组织及其环境。".into(), bbox: None },
            Line { y: 110.0, text: "组织应识别相关方。".into(), bbox: None },
        ];
        assert_eq!(merge_into_paragraphs(&cn).len(), 2, "（一）是 title_level 标题，独段优先");
        // 无标记正文行照旧合并
        let plain = vec![
            Line { y: 100.0, text: "第一段。".into(), bbox: None },
            Line { y: 110.0, text: "第二行内容。".into(), bbox: None },
        ];
        assert_eq!(merge_into_paragraphs(&plain)[0].text, "第一段。第二行内容。");
    }

    /// #11b：连字符合并（两行拼回一行）→ 几何取并集，不是取第一行。
    #[test]
    fn postprocess_boxed_merges_hyphen_and_unions_boxes() {
        let lines = vec![
            Line { y: 100.0, text: "mainten-".into(), bbox: Some((10.0, 90.0, 100.0, 110.0)) },
            Line { y: 120.0, text: "ance".into(), bbox: Some((10.0, 80.0, 120.0, 130.0)) },
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
}
