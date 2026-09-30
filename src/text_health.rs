//! 文本健康检查：乱码字符检测（PDF / OFD 共用）+ 标题级别判定（三通路统一）。
//!
//! 坏字符三类：U+FFFD 替换符、私有区 U+E000..=U+F8FF、控制字符。判定阈值
//! （`min_total` / `bad_percent`）由本模块的
//! [`GARBLED_MIN_TOTAL_CHARS`] / [`GARBLED_BAD_PERCENT_THRESHOLD`] 统一提供，
//! PDF 与 OFD 共用同一阈值（50 字符 / 20% 占比），常量集中于此，防异名漂移。

use crate::reading_order;
use crate::region::{Region, RegionKind};

/// 标题最大行宽：超过此字符数视为正文，不赋标题级别（编号启发式分支）。
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

/// 标题级别判定（PDF 文字层 / OFD 文字层 / gfm OCR 三通路统一，#6 第 2 步）。
///
/// 返回与 `lines` 等长的级别向量（`Some(1..=6)` = 标题行）。**本函数不写 `#`
/// 前缀**——级别是 IR 数据（`Region.heading_level`），字面量由
/// [`Region::rendered_line`](crate::region::Region::rendered_line) 在渲染时写出。
///
/// 规则（按序，命中即返回、跳过后续）：
/// 1. 行判定视图（`trim_start` 后）已以 `#` 开头 → 级别 = 字面量 `#` 段数，
///    文本原样保留（防双重标记；这一分支只贡献"是标题"这件事，不再生成新前缀）；
/// 2. `numbering` 为真且行 ≤ [`TITLE_MAX_CHARS`] 字符、且 `reading_order::title_level`
///    编号启发式命中 → 该启发式给出的级别（PDF/OFD 文字层无布局标题信号，
///    仅此启发式；gfm 传 `numbering=false` 不抹平其布局驱动差异）；
/// 3. 命中 `title_hints`（`(文本, 级别)`，由布局模型或外部提供）→ 对应级别；
/// 4. 均不命中 → `None`。
///
/// `title_hints` 为空（PDF/OFD 文字层）+ `numbering=true` 即纯编号启发式；
/// `title_hints` 非空（gfm 布局标题）+ `numbering=false` 即纯布局驱动。
///
/// 与旧的 `apply_title_prefixes`（已删）逐字节等价：旧函数对 2/3 命中项返回
/// `"#".repeat(lv) + " " + line`、对 1/4 返回 `line.clone()`，而 `rendered_line`
/// 对 1 恰好不写前缀（文本自带字面量），故两者输出同一字符串。
///
/// 这里曾另有一个 `styled: bool` 分支（判定前剥掉 `ANYDOC_RICH_TEXT` 注入的行内
/// 样式标记，配套 `strip_inline_style_markers`）。该开关随 #6 决策 (c) 废弃，且
/// 它是仓内**唯一**会让正文带上 `**`/`<u>` 的来路——现在没有 producer 产出行内
/// 样式标记，"先剥再判"就无从谈起，故连判定视图一起删掉，不留死开关。
/// 第 4 步把样式做成结构化 `Span` 之后，"标题判定要看渲染前的文本"这个需求
/// 会以更合适的形状重新出现（那时读的是 spans 而不是正则剥字符串），实现可
/// `git log -p -- src/text_health.rs` 取回。
/// 字号信号赋级阈值（#11c-v3 附票）：行字号 / 本页中位字号 >= 它 → 文档标题
/// （level 1，对齐 MinerU `doc_title` 的 `#`）。
///
/// GJB 第 1 页真值：行字号 {10.02, 13.02, 13.02, 13.98, 16.02, 25.98} → 中位
/// 13.98（封面正文行少，中位被大字抬高，故不是"26/10=2.6"那种理想比），封面
/// 主标题「质量管理体系要求」25.98/13.98 = **1.858**，MinerU 4.0.8 真 CLI 给
/// `#`（doc_title）。取 1.8 命中它，并与下界 1.6（见 [`TITLE_FONT_RATIO`]）
/// 之间留 0.26 间隔。
pub const DOC_TITLE_FONT_RATIO: f32 = 1.8;
/// 字号信号赋级阈值：>= 它 → 无编号小节标题（level 2，对齐 MinerU
/// `paragraph_title` 的 `##`）。
///
/// **不等于**护栏的 [`crate::reading_order::lines::FONT_SIZE_GUARD_RATIO`]
/// （1.15）：护栏误伤只是多切一段，赋级误伤是凭空造出一个 `#`/`##` 标题
/// （markdown 结构级错误），故赋级取更保守的阈值。GJB 夹逼：
/// 「中央军委装备发展部 颁 布」16.02/13.98 = **1.146**（MinerU 判普通段落）
/// 不触发，「目    次」16.02/10.02 = **1.6**（MinerU 判 `##`）触发。
pub const TITLE_FONT_RATIO: f32 = 1.35;

/// 字号信号补位赋级（#11c-v3 附票）：**只**在 [`title_levels`] 未判出级别
/// （`None`）的行上补——编号/`#` 字面量/hints 判定全部不动，零回归面。
///
/// 动机：MinerU 4.0.8 basic 靠版面模型给 `doc_title`/`paragraph_title` 类型，
/// 本仓文字层无版面模型 → 无编号标题此前只能落在 paragraph（v3 护栏已让它
/// 独段，但无级别）。GJB 前 8 页真 CLI 对照：`# 质量管理体系要求` /
/// `## 目 次` / `## 前 言`——本函数复现同样的级别分配（块边界 6/6 一致）。
///
/// 字号缺失（OCR 通路 / 表格占位 → `None`）不补位；有效字号样本 < 3 时
/// 中位不可信，同样不补。
pub fn merge_font_levels(levels: Vec<Option<u8>>, sizes: &[Option<f32>]) -> Vec<Option<u8>> {
    let mut vals: Vec<f32> = sizes.iter().filter_map(|s| *s).filter(|v| *v > 0.0).collect();
    if vals.len() < 3 {
        return levels;
    }
    vals.sort_by(|a, b| a.total_cmp(b));
    let med = vals[vals.len() / 2];
    levels
        .into_iter()
        .zip(sizes)
        .map(|(lv, &sz)| {
            lv.or_else(|| match sz {
                Some(s) if s >= med * DOC_TITLE_FONT_RATIO => Some(1),
                Some(s) if s >= med * TITLE_FONT_RATIO => Some(2),
                _ => None,
            })
        })
        .collect()
}

pub fn font_levels(sizes: &[Option<f32>]) -> Vec<Option<u8>> {
    merge_font_levels(vec![None; sizes.len()], sizes)
}

pub fn title_levels(
    lines: &[String],
    title_hints: &[(String, usize)],
    numbering: bool,
) -> Vec<Option<u8>> {
    lines
        .iter()
        .map(|line| {
            // 规则 1：文本自带 markdown 字面量——级别取自字面量，不再叠加。
            if let Some(lv) = Region::leading_hash_level(line.trim_start()) {
                return Some(lv);
            }
            if numbering
                && line.chars().count() <= TITLE_MAX_CHARS
                && let Some(lv) = reading_order::title_level(line)
            {
                return Some(lv as u8);
            }
            if !title_hints.is_empty() {
                let lt = line.trim();
                for (tt, lv) in title_hints {
                    if lt == tt.as_str() || lt.contains(tt.as_str()) || tt.contains(lt) {
                        return Some(*lv as u8);
                    }
                }
            }
            None
        })
        .collect()
}

/// 标题级别判定 + 造 Body 区块（三通路共用尾步）：`levels` 与 `lines` 同序配对。
///
/// 长度不等是调用方的编程错误（levels 来自 `title_levels(lines,…)`），这里
/// 按较短者对齐并 `debug_assert`，不在 release 路径 panic 掉整篇转换。
///
/// #11b 真相函数：几何透传给 content_list v2 的 bbox 投影。#11b-v2 后文字层
/// 通路也走 boxed 链，`Vec<String>` 薄封装已删（真相只有一份）。
pub(crate) fn body_regions_boxed(
    lines: Vec<crate::reading_order::Line>,
    levels: Vec<Option<u8>>,
) -> Vec<crate::region::Region> {
    debug_assert_eq!(lines.len(), levels.len(), "levels 必须由同一 lines 算出");
    lines
        .into_iter()
        .zip(levels)
        .map(|(l, level)| {
            let (x0, x1, y0, y1) = l.bbox.unwrap_or((0.0, 0.0, 0.0, 0.0));
            let mut r = Region::new(x0, x1, y0, y1, l.text).with_heading_level(level);
            // #10 切片 4：段落以列表 marker 开头 → list item 标注。三通路共用
            // 本转换点（OCR gfm_adapter / OFD mod.rs / PDF text_layer），与切片 1
            // 的 merge 独段护栏同判据同文本视图。标题行不打——赋了级别的行不是
            // 列表项。渲染层不感知（markdown 零变化），content_list v2 消费。
            if level.is_none() && reading_order::starts_with_list_marker(&r.text) {
                r.list_item = true;
            }
            // #10 切片 5：目录块成员标记回传（Line::no_merge → Region）。
            // 合并阶段已凭它逐条独立，此处把事实带回 Region 供 INDEX 回贴。
            r.index_member = l.no_merge;
            r
        })
        .collect()
}

/// #10 INDEX 类型票：把目次（INDEX）条目行从 `Body` 升格为
/// [`RegionKind::Index`]，**同时清掉标题级别**（MinerU 的 index item 不带
/// `level`；且 `- ` 列表项若再写 `#` 前缀，GFM 会读成标题而非列表项）。
///
/// 只对**点线行**打标（[`crate::reading_order::lines::is_index_entry`]）：
/// MinerU 靠版面模型把整个目录块判为一个 `IndexBlock`（可含不带点线的条目），
/// 本仓文字层无版面模型，点线形态是唯一拿得到的 INDEX 信号——故本函数是
/// **文字层专属**，OCR 通路刻意不调用（它的正途是接版面模型的 INDEX 类型）。
pub(crate) fn mark_index_entries(regions: Vec<Region>) -> Vec<Region> {
    regions
        .into_iter()
        .map(|mut r| {
            if r.kind == RegionKind::Body
                && crate::reading_order::lines::is_index_entry(&r.text)
            {
                r.kind = RegionKind::Index;
                r.heading_level = None;
            }
            r
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

    /// 便捷视图：`title_levels` 的结果按旧方式落到行字面量上（= producer 赋
    /// 级别 + 渲染层写前缀的合成视图），供各单测沿用"看字符串"的断言写法。
    fn render_lines(lines: &[String], levels: &[Option<u8>]) -> Vec<String> {
        body_regions_boxed(crate::reading_order::Line::from_texts(lines.to_vec()), levels.to_vec())
            .into_iter()
            .map(|r| r.rendered_line().into_owned())
            .collect()
    }

    /// #10 切片 4：list_item 标注——marker 行且无标题级别 → true；
    /// 标题行（有级别）与普通行 → false。
    #[test]
    fn list_item_flag_marks_marker_paragraphs_only() {
        let lines = vec!["f)  确定产品通用化要求".to_string(), "4.2 组织环境".to_string()];
        let levels = vec![None, Some(2)];
        let rs = body_regions_boxed(crate::reading_order::Line::from_texts(lines), levels);
        assert!(rs[0].list_item, "marker 行且无级别 → list_item");
        assert!(!rs[1].list_item, "赋了级别的标题行不是列表项");
        let plain = body_regions_boxed(
            crate::reading_order::Line::from_texts(vec!["普通正文行".to_string()]),
            vec![None],
        );
        assert!(!plain[0].list_item);
    }

    #[test]
    fn numbering_only_when_flagged() {
        // numbering=true 空 hints：编号启发式命中赋级别；正文行不赋。
        let lines = vec![
            "一、总则".to_string(),
            "这是正文第一句。".to_string(),
            "1.1 适用范围".to_string(),
        ];
        let out = render_lines(&lines, &title_levels(&lines, &[], true));
        assert_eq!(out[0], "## 一、总则");
        assert_eq!(out[1], "这是正文第一句。");
        assert_eq!(out[2], "### 1.1 适用范围");
        // numbering=false：即使编号也不赋级别（gfm 布局驱动语义）
        let out = render_lines(&lines, &title_levels(&lines, &[], false));
        assert_eq!(out, lines);
    }

    #[test]
    fn layout_hints_drive_levels_without_numbering() {
        // 空 hints + numbering=false 时，靠传入的布局提示赋级别。
        let lines = vec!["究极标题".to_string(), "普通正文".to_string()];
        let hints = vec![("究极标题".to_string(), 2)];
        let out = render_lines(&lines, &title_levels(&lines, &hints, false));
        assert_eq!(out[0], "## 究极标题");
        assert_eq!(out[1], "普通正文");
    }

    #[test]
    fn existing_hash_prefix_is_not_doubled() {
        // 规则 1：来源文本自带 `#` 字面量 → 级别取自字面量、渲染时不重复写前缀。
        let lines = vec!["# 已带前缀".to_string(), "一、小节".to_string()];
        let levels = title_levels(&lines, &[], true);
        assert_eq!(levels, vec![Some(1), Some(2)], "两条都是标题行");
        assert_eq!(
            render_lines(&lines, &levels),
            vec!["# 已带前缀", "## 一、小节"]
        );
    }

    /// #6 第 2 步的核心等价：`title_levels` + `Region::rendered_line` 必须与
    /// 改造前的 `apply_title_prefixes`（已删，见 git log -p）**输出同一批字符串**。
    /// 期望值按旧函数逐条手算，覆盖三条规则 + 规则 1/2 叠加 + 超长行 + 前导空白。
    #[test]
    fn levels_then_render_equals_legacy_prefixes() {
        // (行, hints, numbering, 期望渲染结果)——期望值按旧 apply_title_prefixes 手算。
        let long = format!("1. {}", "长".repeat(TITLE_MAX_CHARS));
        let cases: Vec<(Vec<String>, Vec<(String, usize)>, bool, Vec<String>)> = vec![
            // 编号命中 / 正文 / 更深层级
            (
                vec!["一、总则".into(), "这是正文。".into(), "2.1.1 细目".into()],
                vec![],
                true,
                vec!["## 一、总则".into(), "这是正文。".into(), "#### 2.1.1 细目".into()],
            ),
            // hints 驱动（numbering=false，布局通路语义）
            (
                vec!["适用范围".into(), "正文".into()],
                vec![("适用范围".to_string(), 3)],
                false,
                vec!["### 适用范围".into(), "正文".into()],
            ),
            // 规则 1 优先：已带字面量的行不叠前缀，即使编号也命中
            (
                vec!["## 一、总则".into()],
                vec![],
                true,
                vec!["## 一、总则".into()],
            ),
            // 超长行（> TITLE_MAX_CHARS）不赋级别
            (vec![long.clone()], vec![], true, vec![long]),
        ];
        for (lines, hints, numbering, expected) in cases {
            let got = render_lines(&lines, &title_levels(&lines, &hints, numbering));
            assert_eq!(got, expected, "lines={lines:?} numbering={numbering}");
        }
        // 前导空白 + `#`：判定视图（trim_start）认它是标题 → 不叠前缀；渲染视图
        // 不 trim → 与旧 render.rs 的 `t.starts_with('#')` 同判为非标题行（旧行为）。
        let lines = vec!["  # 缩进的标题".to_string()];
        let regions = body_regions_boxed(
            crate::reading_order::Line::from_texts(lines.clone()),
            title_levels(&lines, &[], true),
        );
        assert_eq!(regions[0].rendered_line(), "  # 缩进的标题");
        assert!(!regions[0].is_heading(), "旧 render 口径：不 trim，故非标题");
        assert!(regions[0].is_heading_trimmed(), "判定口径：trim 后是标题");
    }

    // ── ANYDOC_RICH_TEXT 废弃后的口径（#6 决策 (c)）──

    #[test]
    fn literal_style_markers_are_no_special_case() {
        // 废弃前 `styled=true` 会先把首尾成对的 `**` 剥掉再判标题；现在没有
        // producer 产出行内样式标记，故这类文本按**普通行**处理：以 `*` 开头
        // 命中不了编号启发式，既不剥标记也不因剥标记而赋级别。
        let lines = vec!["**一、总则**".to_string(), "普通**正文**".to_string()];
        assert_eq!(title_levels(&lines, &[], true), vec![None, None]);
        // 真编号标题仍照常命中（证明不是"整个启发式被删坏了"）。
        let real = vec!["一、总则".to_string()];
        assert_eq!(title_levels(&real, &[], true), vec![Some(2)]);
    }

    // ── 字号信号补位赋级（#11c-v3 附票）──

    #[test]
    fn font_levels_follow_gjb_page1_distribution() {
        // GJB 第 1 页实测：封面主标题 26pt / 「目 次」16pt / 正文 10pt，
        // 中位取 10 → 26/10=2.6 >= 2.0 判文档标题、16/10=1.6 >= 1.15 判小节。
        let sizes = [Some(26.0f32), Some(16.0), Some(10.0), Some(10.0), Some(10.0)];
        assert_eq!(
            font_levels(&sizes),
            vec![Some(1), Some(2), None, None, None]
        );
    }

    #[test]
    fn font_levels_keeps_existing_levels_untouched() {
        // 补位只在 None 处发生：编号/字面量/hints 已判的级别一律不动。
        let sizes = [Some(10.0f32), Some(26.0), Some(10.0)];
        let levels = vec![Some(3), None, Some(2)];
        assert_eq!(
            merge_font_levels(levels, &sizes),
            vec![Some(3), Some(1), Some(2)]
        );
    }

    #[test]
    fn font_levels_ignores_missing_and_thin_samples() {
        // OCR 通路恒 None → 整页不补位（红线）。
        let none_page = [None, None, None, None];
        assert_eq!(font_levels(&none_page), vec![None; 4]);
        // 有效样本 < 3 → 中位不可信，不补位（哪怕比值很大）。
        let thin = [Some(10.0f32), Some(30.0), None];
        assert_eq!(font_levels(&thin), vec![None; 3]);
    }

    #[test]
    fn font_levels_threshold_boundary() {
        // GJB 真值夹逼的两侧：1.146（中央军委装备发展部，MinerU 判普通段落）
        // 不触发、1.6（目 次，MinerU 判 ##）触发小节级、1.858（封面主标题，
        // MinerU 判 #）触发文档级。
        let base = [Some(13.98f32); 5];
        let mut below = base;
        below[0] = Some(16.02);
        assert_eq!(font_levels(&below)[0], None, "1.146x 不算标题");
        let mut mid = base;
        mid[0] = Some(22.37);
        assert_eq!(font_levels(&mid)[0], Some(2), "1.6x 算小节标题");
        let mut top = base;
        top[0] = Some(25.98);
        assert_eq!(font_levels(&top)[0], Some(1), "1.858x 算文档标题");
    }
}
