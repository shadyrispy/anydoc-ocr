//! 文本区域的阅读顺序还原（双列/多列感知排序）。
//!
//! OCR 通路（`gfm_adapter`）与文字层通路（`pdf/mod.rs`）共用同一排序算法：
//! 把所有区域按 x 中心排序，取**最大间隙**切分出列——双列页面的列间 gutter
//! 正是最大间隙；单栏页面的最大间隙很小（只是列内相邻行的 x 抖动），不触发
//! 切分。每列内按 y 排序、列间从左到右。
//!
//! 关键点：列检测用**所有**文本区域（含宽条目）。早先版本把跨度>60% 页宽的
//! 元素当作"整宽"剔除，但公报目录里左列的长标题（如"上海市人民政府办公厅
//! 关于转发市交通委制订的《上海港口基础设施维护管理办法》的通知"）跨度就达
//! ~68% 页宽，被误删后列间隙被糊掉、分栏失败。因此只把**真正跨整页**（x 同时
//! 贴近左右边距）的元素（页眉/页脚/通栏标题）剔除为 header/footer，左列长条目
//! 按其 x 中心自然归入左列。
//!
//! 无清晰间隙（单栏或无法切分）时退化为纯 y 排序，兼容单栏文档与边缘情况。
//!
//! `regions`: `(x_min, x_max, y_min, y_max, 文本)`。

/// OCR 文本区域的阅读顺序还原。
///
/// `y` 语义：**越小越靠上**（图像坐标系，原点左上）。PDF 坐标（原点左下）
/// 需在调用方翻转后传入，否则上下颠倒。
pub fn order_text_regions(regions: &[(f32, f32, f32, f32, String)]) -> Vec<String> {
    if regions.is_empty() {
        return Vec::new();
    }
    let page_w = regions.iter().map(|r| r.1).fold(0.0_f32, f32::max);
    if page_w <= 0.0 {
        return sort_by_y(regions);
    }
    let Some(split) = detect_column_split(regions) else {
        return sort_by_y(regions);
    };

    // 正文区域：(中心x, y, 文本)；跨整页元素：(y, 文本)
    let is_full = |r: &(f32, f32, f32, f32, String)| {
        (r.1 - r.0) > 0.92 * page_w && r.0 < 0.08 * page_w
    };
    let mut left: Vec<(f32, String)> = regions
        .iter()
        .filter(|r| !is_full(r) && ((r.0 + r.1) / 2.0) < split)
        .map(|r| (r.2, r.4.clone()))
        .collect();
    let mut right: Vec<(f32, String)> = regions
        .iter()
        .filter(|r| !is_full(r) && ((r.0 + r.1) / 2.0) >= split)
        .map(|r| (r.2, r.4.clone()))
        .collect();
    left.sort_by(ord_y);
    right.sort_by(ord_y);

    // 整宽元素按 y 归页眉(y<正文起点)/页脚(y>正文终点)/正文区间(罕见置后)
    let full: Vec<(f32, String)> = regions
        .iter()
        .filter(|r| is_full(r))
        .map(|r| (r.2, r.4.clone()))
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
    let mut head: Vec<_> = full.iter().filter(|(y, _)| *y < body_min).cloned().collect();
    let mut foot: Vec<_> = full.iter().filter(|(y, _)| *y > body_max).cloned().collect();
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
pub fn detect_column_split(regions: &[(f32, f32, f32, f32, String)]) -> Option<f32> {
    if regions.len() < 4 {
        return None;
    }
    let page_w = regions.iter().map(|r| r.1).fold(0.0_f32, f32::max);
    if page_w <= 0.0 {
        return None;
    }
    let is_full = |r: &(f32, f32, f32, f32, String)| {
        (r.1 - r.0) > 0.92 * page_w && r.0 < 0.08 * page_w
    };
    let mut body: Vec<f32> = regions
        .iter()
        .filter(|r| !is_full(r))
        .map(|r| (r.0 + r.1) / 2.0)
        .collect();
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
    (best_gap >= min_gap && best_i >= 2 && (body.len() - best_i) >= 2)
        .then(|| (body[best_i - 1] + body[best_i]) / 2.0)
}

fn ord_y(a: &(f32, String), b: &(f32, String)) -> std::cmp::Ordering {
    a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal)
}

/// 单栏/无可切分列时的退化为纯 y 排序（保持旧行为兼容）
fn sort_by_y(regions: &[(f32, f32, f32, f32, String)]) -> Vec<String> {
    let mut v: Vec<(f32, String)> = regions.iter().map(|r| (r.2, r.4.clone())).collect();
    v.sort_by(ord_y);
    v.into_iter().map(|(_, t)| t).collect()
}

#[cfg(test)]
mod tests {
    use super::order_text_regions;

    /// 构造区域：(x_min, x_max, y_min, y_max=+10, 文本)
    fn reg(x0: f32, x1: f32, y0: f32, t: &str) -> (f32, f32, f32, f32, String) {
        (x0, x1, y0, y0 + 10.0, t.to_string())
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

    #[test]
    fn pdf_y_coordinates_flipped_sort_top_down() {
        // PDF 坐标原点左下：y 大=靠上。翻转 y（-y）后排序应仍上→下。
        let regions = vec![
            reg(50.0, 450.0, -300.0, "top"),
            reg(50.0, 450.0, -100.0, "bottom"),
            reg(50.0, 450.0, -200.0, "middle"),
        ];
        assert_eq!(order_text_regions(&regions), vec!["top", "middle", "bottom"]);
    }
}
