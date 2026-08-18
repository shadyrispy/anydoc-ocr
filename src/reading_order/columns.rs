//! 区域驱动的列检测与排序（三级降级链的末级兜底）。
//!
//! 把所有区域按 x 中心排序，取最大间隙切分出列；每列内按 y 排序、列间从左到右。

use crate::region::Region;

/// OCR 文本区域的阅读顺序还原。
///
/// `y` 语义：**越小越靠上**（图像坐标系，原点左上）。PDF 坐标（原点左下）
/// 需在调用方翻转后传入，否则上下颠倒。
pub fn order_text_regions(regions: &[Region]) -> Vec<String> {
    if regions.is_empty() {
        return Vec::new();
    }
    let page_w = Region::page_w(regions);
    if page_w <= 0.0 {
        return sort_by_y(regions);
    }
    let Some(split) = detect_column_split(regions) else {
        return sort_by_y(regions);
    };

    // 列分类：左/右/整宽三组（复用 split_columns，消除与 order_within_block 的重复）
    let (left_refs, right_refs, full_refs) = split_columns(regions, split);
    let mut left: Vec<(f32, String)> = left_refs
        .iter()
        .map(|r| (r.y_min, r.text.clone()))
        .collect();
    let mut right: Vec<(f32, String)> = right_refs
        .iter()
        .map(|r| (r.y_min, r.text.clone()))
        .collect();
    left.sort_by(ord_y);
    right.sort_by(ord_y);

    // 整宽元素按 y 归页眉(y<正文起点)/页脚(y>正文终点)/正文区间(罕见置后)
    let full: Vec<(f32, String)> = full_refs
        .iter()
        .map(|r| (r.y_min, r.text.clone()))
        .collect();
    let body_min = left
        .iter()
        .chain(right.iter())
        .map(|(y, _)| *y)
        .fold(f32::INFINITY, f32::min);
    let body_max = left
        .iter()
        .chain(right.iter())
        .map(|(y, _)| *y)
        .fold(f32::NEG_INFINITY, f32::max);
    let mut head: Vec<_> = full
        .iter()
        .filter(|(y, _)| *y < body_min)
        .cloned()
        .collect();
    let mut foot: Vec<_> = full
        .iter()
        .filter(|(y, _)| *y > body_max)
        .cloned()
        .collect();
    let mut mid: Vec<_> = full
        .iter()
        .filter(|(y, _)| *y >= body_min && *y <= body_max)
        .cloned()
        .collect();
    head.sort_by(ord_y);
    mid.sort_by(ord_y);
    foot.sort_by(ord_y);

    let mut out: Vec<String> = Vec::new();
    for (_, t) in head {
        out.push(t);
    }
    for (_, t) in left {
        out.push(t);
    }
    for (_, t) in right {
        out.push(t);
    }
    for (_, t) in mid {
        out.push(t);
    }
    for (_, t) in foot {
        out.push(t);
    }
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

fn ord_y(a: &(f32, String), b: &(f32, String)) -> std::cmp::Ordering {
    a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal)
}

/// 单栏/无可切分列时的退化为纯 y 排序（保持旧行为兼容）
fn sort_by_y(regions: &[Region]) -> Vec<String> {
    let mut v: Vec<(f32, String)> = regions.iter().map(|r| (r.y_min, r.text.clone())).collect();
    v.sort_by(ord_y);
    v.into_iter().map(|(_, t)| t).collect()
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
}
