//! 区域驱动的列检测与排序（三级降级链的末级兜底）。
//!
//! 把所有区域按 x 中心排序，取最大间隙切分出列；每列内按 y 排序、列间从左到右。

use super::lines::{Line, resolve_line_boundary};
use crate::region::Region;

/// OCR 文本区域的阅读顺序还原。
///
/// `y` 语义：**越小越靠上**（图像坐标系，原点左上）。PDF 坐标（原点左下）
/// 需在调用方翻转后传入，否则上下颠倒。
pub fn order_text_regions(regions: &[Region]) -> Vec<String> {
    order_text_regions_boxed(regions).into_iter().map(|l| l.text).collect()
}

/// 同上，**带几何**版（#11b 真相函数）：OFD/PDF 文字层通路消费
/// （`ofd/mod.rs` 的 `PageData::Text`、`pdf/text_layer.rs` 的 `build_body_regions`），
/// 几何一路带到 Region 上 → content_list v2 的 bbox 投影。
pub(crate) fn order_text_regions_boxed(regions: &[Region]) -> Vec<Line> {
    if regions.is_empty() {
        return Vec::new();
    }
    // #10 切片 5：目录块（任一行带 `index_member`）**禁列切分**，纯 y 排序。
    // 目录页的"编号+标题 …… 页码"会被 `detect_column_split` 的最大间隙逻辑误
    // 判成双列（窄编号行归左列、宽条目行归整宽）→ 输出顺序 mid→left 交错，
    // 实测 `4.1 理解组织…2` / `理解相关方…2` / `4.2`（编号跑到条目后）。目录
    // 不是分栏排版，y 序就是阅读序。
    if regions.iter().any(|r| r.index_member) {
        return sort_by_row_boxed(regions);
    }
    let page_w = Region::page_w(regions);
    if page_w <= 0.0 {
        return sort_by_y_boxed(regions);
    }
    let Some(split) = detect_column_split(regions) else {
        return sort_by_y_boxed(regions);
    };

    // 列分类：左/右/整宽三组（复用 split_columns，消除与 order_within_block 的重复）
    let (left_refs, right_refs, full_refs) = split_columns(regions, split);
    let mut left: Vec<Line> = left_refs.into_iter().map(Line::from_region).collect();
    let mut right: Vec<Line> = right_refs.into_iter().map(Line::from_region).collect();
    left.sort_by(ord_y);
    right.sort_by(ord_y);

    // 整宽元素按 y 归页眉(y<正文起点)/页脚(y>正文终点)/正文区间(罕见置后)
    let full: Vec<Line> = full_refs.into_iter().map(Line::from_region).collect();
    let body_min = left
        .iter()
        .chain(right.iter())
        .map(|l| l.y)
        .fold(f32::INFINITY, f32::min);
    let body_max = left
        .iter()
        .chain(right.iter())
        .map(|l| l.y)
        .fold(f32::NEG_INFINITY, f32::max);
    let mut head: Vec<_> = full.iter().filter(|l| l.y < body_min).cloned().collect();
    let mut foot: Vec<_> = full.iter().filter(|l| l.y > body_max).cloned().collect();
    let mut mid: Vec<_> = full
        .iter()
        .filter(|l| l.y >= body_min && l.y <= body_max)
        .cloned()
        .collect();
    head.sort_by(ord_y);
    mid.sort_by(ord_y);
    foot.sort_by(ord_y);

    let mut out: Vec<Line> = Vec::new();
    out.extend(head);
    out.extend(left);
    out.extend(right);
    out.extend(mid);
    out.extend(foot);
    out
}

/// 检测双列切分线（gutter）。返回 `None` 表示单栏/无法切分。
///
/// 列检测用**所有**正文区域（含宽条目）的中心 x，取最大间隙切分；要求间隙
/// >= 3% 页宽且两侧各 >=2 区域，避免把单栏内的大间距误判为分栏。真正跨整页
/// （x 同时贴近左右边距）的元素（页眉/页脚/通栏标题）先剔除。
pub fn detect_column_split(regions: &[Region]) -> Option<f32> {
    if regions.len() < 4 {
        return None;
    }
    let page_w = Region::page_w(regions);
    if page_w <= 0.0 {
        return None;
    }
    let body_regions: Vec<&Region> = regions
        .iter()
        .filter(|r| !r.is_full_width(page_w))
        .collect();
    let mut body: Vec<f32> = body_regions.iter().map(|r| r.center_x()).collect();
    if body.len() < 4 {
        return None;
    }
    body.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut best_gap = 0.0_f32;
    let mut best_i = 1usize;
    for i in 1..body.len() {
        let g = body[i] - body[i - 1];
        if g > best_gap {
            best_gap = g;
            best_i = i;
        }
    }
    let min_gap = 0.03 * page_w;
    if best_gap < min_gap || best_i < 2 || (body.len() - best_i) < 2 {
        return None;
    }
    let split = (body[best_i - 1] + body[best_i]) / 2.0;
    // 傀栏 vs 单栏判别：真实列间隙是一段无文本的竖直空白带——页内没有任何区域
    // 跨过该中线（左列区域右缘 < split、右列区域左缘 > split）。单栏页行宽天然
    // 变化（短标签 + 通栏段落），最大 center_x 间隙往往是相邻两行的宽度差，
    // 全宽正文区域会跨过该"假间隙"。若存在跨过分隔线的区域 → 非真列 → 单栏。
    // 修复：9001c 文字版 4.1/4.2 节正文大量缺行（误判双列后正文被颠倒/切割）。
    let bridges = body_regions
        .iter()
        .any(|r| r.x_min < split && split < r.x_max);
    if bridges {
        return None;
    }
    Some(split)
}

fn ord_y(a: &Line, b: &Line) -> std::cmp::Ordering {
    a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal)
}

/// 单栏/无可切分列时的退化为纯 y 排序（保持旧行为兼容）。
pub(super) fn sort_by_y_boxed(regions: &[Region]) -> Vec<Line> {
    let mut v: Vec<Line> = regions.iter().map(Line::from_region).collect();
    v.sort_by(ord_y);
    v
}

/// #10 切片 5：目录块的**行簇 + 行内 x** 排序（禁列切分的配套真相）。
///
/// 为什么不能纯 y 排：扫描件常有 1–3° 倾斜，同一视觉行内各框的 `y_min` 会差
/// 几个像素——实测 nuaa_tupian.pdf 目录页 `4.2`（y_min 408，x 74–109）与
/// `理解相关方的需求和期望……2`（y_min **406**，x 102–717）属于同一行，纯 y
/// 排序把编号甩到条目文字**之后**（`- 理解相关方…` 后跟一个光秃秃的 `- 4.2`）。
/// 目录条目是"编号 → 标题 → 页码"的横向序列，行内必须按 x 走。
///
/// 做法：y 升序扫描聚类（相邻 y 差 <= 行高中位数的一半判为**同一视觉行**），
/// 簇内按 `x_min` 升序，簇间按簇首 y 升序。仅目录块启用——正文的倾斜错位是
/// 另一个票，本次不动（避免 golden 面扩散）。
pub(super) fn sort_by_row_boxed(regions: &[Region]) -> Vec<Line> {
    if regions.is_empty() {
        return Vec::new();
    }
    let mut items: Vec<(f32, f32, Line)> = regions
        .iter()
        .map(|r| (r.y_min, r.x_min, Line::from_region(r)))
        .collect();
    items.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    // 行高中位数（region 高度；退化框兜底 1.0，防除零/全零 tol）
    let mut hs: Vec<f32> = regions.iter().map(|r| (r.y_max - r.y_min).max(1.0)).collect();
    hs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let tol = hs.get(hs.len() / 2).copied().unwrap_or(20.0) * 0.5;

    let mut out: Vec<Line> = Vec::with_capacity(items.len());
    let mut row: Vec<(f32, f32, Line)> = Vec::new();
    let mut row_y = items[0].0;
    for it in items {
        if !row.is_empty() && it.0 - row_y > tol {
            flush_index_row(&mut row, &mut out);
            row_y = it.0;
        }
        row.push(it);
    }
    flush_index_row(&mut row, &mut out);
    out
}

/// 一个视觉行簇 → **一条**目录条目：簇内按 x 升序拼接（与 [`Line::union_bbox`]
/// 取几何并集），文本边界按 `resolve_line_boundary`（docvortex 口径）。
///
/// 为什么要拼：PP-OCR det 常把"编号 + 标题"拆成两个框（`4.2` x74–109 与
/// `理解相关方的需求和期望……2` x102–717 属同一行）→ 不拼就出两条
/// （`- 4.2` / `- 理解相关方…`），而 MinerU 是一条
/// （`- 4.2 理解相关方的需求和期望………2`）。横向序列拼回一条才对齐。
fn flush_index_row(row: &mut Vec<(f32, f32, Line)>, out: &mut Vec<Line>) {
    row.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut iter = row.drain(..);
    let Some((_, _, mut cur)) = iter.next() else {
        return;
    };
    for (_, _, next) in iter {
        let next_text = next.text.trim_start();
        if next_text.is_empty() {
            continue;
        }
        let (head, sep) = resolve_line_boundary(&cur.text, &next.text);
        cur.text = head;
        cur.text.push_str(sep);
        cur.text.push_str(next_text);
        cur.bbox = Line::union_bbox(cur.bbox, next.bbox);
    }
    out.push(cur);
}

/// 将 region 按列切分线 `split` 分为左/右/整宽三组（消除 `order_text_regions` 与
/// `order_within_block` 的列分类重复）。
pub(super) fn split_columns<'a>(
    regions: &'a [Region],
    split: f32,
) -> (Vec<&'a Region>, Vec<&'a Region>, Vec<&'a Region>) {
    let page_w = Region::page_w(regions);
    let mut left = Vec::new();
    let mut right = Vec::new();
    let mut full = Vec::new();
    for r in regions {
        if r.is_full_width(page_w) {
            full.push(r);
        } else if r.center_x() < split {
            left.push(r);
        } else {
            right.push(r);
        }
    }
    (left, right, full)
}

#[cfg(test)]
mod tests {
    use super::order_text_regions;
    use crate::region::Region;

    /// 构造区域：(x_min, x_max, y_min, y_max=+10, 文本)
    fn reg(x0: f32, x1: f32, y0: f32, t: &str) -> Region {
        Region::new(x0, x1, y0, y0 + 10.0, t)
    }

    #[test]
    fn two_column_left_then_right() {
        // 页宽 1000：左列 50..450，右列 550..950，中间 12% 间隙
        let regions = vec![
            reg(50.0, 450.0, 100.0, "L1"),
            reg(50.0, 450.0, 200.0, "L2"),
            reg(50.0, 450.0, 300.0, "L3"),
            reg(550.0, 950.0, 100.0, "R1"),
            reg(550.0, 950.0, 200.0, "R2"),
            reg(550.0, 950.0, 300.0, "R3"),
        ];
        assert_eq!(
            order_text_regions(&regions),
            vec!["L1", "L2", "L3", "R1", "R2", "R3"]
        );
    }

    #[test]
    fn fullwidth_header_and_footer_excluded_from_columns() {
        // 整宽页眉(上)/页脚(下)会糊掉列间隙，必须剔除后才能正确分栏
        let regions = vec![
            reg(0.0, 1000.0, 20.0, "HEADER"),
            reg(50.0, 450.0, 100.0, "L1"),
            reg(50.0, 450.0, 200.0, "L2"),
            reg(550.0, 950.0, 100.0, "R1"),
            reg(550.0, 950.0, 200.0, "R2"),
            reg(0.0, 1000.0, 400.0, "FOOTER"),
        ];
        assert_eq!(
            order_text_regions(&regions),
            vec!["HEADER", "L1", "L2", "R1", "R2", "FOOTER"]
        );
    }

    #[test]
    fn single_column_fallback_by_y() {
        let regions = vec![
            reg(50.0, 450.0, 300.0, "A"),
            reg(50.0, 450.0, 100.0, "B"),
            reg(50.0, 450.0, 200.0, "C"),
        ];
        assert_eq!(order_text_regions(&regions), vec!["B", "C", "A"]);
    }

    #[test]
    fn no_interleave_when_columns_present() {
        // 复现真实 bug：左列 1..5 与右列 1..5 逐行交错，须还原为左全→右全
        let regions = vec![
            reg(50.0, 450.0, 100.0, "L1"),
            reg(550.0, 950.0, 105.0, "R1"), // y 相近，旧逻辑会插到 L1 后
            reg(50.0, 450.0, 200.0, "L2"),
            reg(550.0, 950.0, 205.0, "R2"),
            reg(50.0, 450.0, 300.0, "L3"),
            reg(550.0, 950.0, 305.0, "R3"),
        ];
        assert_eq!(
            order_text_regions(&regions),
            vec!["L1", "L2", "L3", "R1", "R2", "R3"]
        );
    }

    #[test]
    fn tight_gutter_two_column() {
        // 双列但 gutter 仅 4%（小于旧 4% 合并阈值），仍应正确分栏
        let regions = vec![
            reg(50.0, 480.0, 100.0, "L1"),
            reg(50.0, 480.0, 200.0, "L2"),
            reg(520.0, 950.0, 100.0, "R1"),
            reg(520.0, 950.0, 200.0, "R2"),
        ];
        assert_eq!(order_text_regions(&regions), vec!["L1", "L2", "R1", "R2"]);
    }

    /// 回归（9001c 文字版 4.1/4.2 正文缺行根因）：单栏页行宽天然变化（短标签 +
    /// 通栏段落），最大 center_x 间隙是相邻两行的宽度差，通栏正文区域跨过该
    /// "假间隙" → 不得判为双列（bridging 判别）。此前误判双列导致正文颠序/丢失。
    #[test]
    fn single_column_full_width_lines_do_not_split() {
        // 短标签行 cx≈100，通栏段落行 cx≈300（跨 75..540 全宽）
        let regions = vec![
            reg(75.0, 540.0, 500.0, "通栏正文一"), // cx=307，跨假间隙
            reg(75.0, 180.0, 400.0, "短标签"),     // cx=127
            reg(75.0, 540.0, 300.0, "通栏正文二"), // cx=307
            reg(75.0, 540.0, 200.0, "通栏正文三"), // cx=307
            reg(75.0, 160.0, 100.0, "短标题4.1"),  // cx=117
        ];
        // 最大 cx 间隙在 127 与 307 之间（gap=180，>3% 页宽），但通栏行跨过该
        // 中线 → bridging → 非列 → 单栏 y 排序，正文不被切割/颠倒。
        assert_eq!(
            order_text_regions(&regions),
            vec![
                "短标题4.1",
                "通栏正文三",
                "通栏正文二",
                "短标签",
                "通栏正文一"
            ],
            "单栏页不得误判双列"
        );
    }

    #[test]
    fn pdf_y_coordinates_flipped_sort_top_down() {
        // PDF 坐标原点左下：y 大=靠上。翻转 y（-y）后排序应仍上→下。
        let regions = vec![
            reg(50.0, 450.0, -300.0, "top"),
            reg(50.0, 450.0, -100.0, "bottom"),
            reg(50.0, 450.0, -200.0, "middle"),
        ];
        assert_eq!(
            order_text_regions(&regions),
            vec!["top", "middle", "bottom"]
        );
    }

    /// #10 切片 5：倾斜扫描件目录页——同一视觉行被 det 拆成"编号框 + 条目框"，
    /// 且条目框的 `y_min` **比编号框更靠上**（倾斜所致）→ 纯 y 排序会把编号甩到
    /// 条目之后。行簇排序按 x 拼回一条。
    #[test]
    fn sort_by_row_boxed_joins_skewed_index_row_by_x() {
        // 复刻 nuaa_tupian.pdf 第 2 页实测：`4.2` y_min 408 / x 74–109，
        // `理解相关方的需求和期望……2` y_min **406**（更靠上）/ x 102–717。
        let mut regions = vec![
            reg(72.0, 719.0, 384.0, "4.1 理解组织及其环境……2"),
            reg(102.0, 717.0, 406.0, "理解相关方的需求和期望……2"),
            reg(74.0, 109.0, 408.0, "4.2"),
            reg(72.0, 719.0, 449.0, "44 质量管理体系及其过程……2"),
        ];
        for r in regions.iter_mut() {
            r.index_member = true;
        }
        let out = super::sort_by_row_boxed(&regions);
        let texts: Vec<&str> = out.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts.len(), 3, "4.2 与条目文字属同一视觉行，须拼成一条：{texts:?}");
        assert_eq!(texts[0], "4.1 理解组织及其环境……2");
        assert!(
            texts[1].starts_with("4.2") && texts[1].contains("理解相关方"),
            "编号必须在条目文字之前：{texts:?}"
        );
        assert_eq!(texts[2], "44 质量管理体系及其过程……2");
    }
}
