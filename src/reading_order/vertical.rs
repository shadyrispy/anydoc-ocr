//! 竖排文本检测与排序（T3，对齐 MinerU `is_vertical_text_block_by_spans`）。
//!
//! OCR 通道：图片型扫描件中的竖排文字（公文竖排标题 / 古籍 / 竖排书页），det 输出
//! 的文本 region 呈「高 >> 宽」的窄条且沿 y 轴排列。按 x 中心聚类成列，列内按 y 排序
//! （每列自上而下），列间按 x 降序（中文竖排从右到左阅读）。
//!
//! 只做**窄高条**的显式判定，避免把通栏/整宽元素误判为竖排；非竖排 regions 由调用方
//! （`blocks::order_structure`）继续走原块驱动排序，竖排段落优先输出。

use crate::region::Region;

/// 竖排判定：region 高 > 该倍数的宽（窄高条）。
const VERTICAL_RATIO: f32 = 2.5;
/// 竖排 region 宽度上限（页宽比例）：避免把通栏/整宽元素误判为竖排。
const MAX_VERTICAL_WIDTH_FRAC: f32 = 0.4;
/// 同列容差：相邻 x 中心距离 ≤ 该条宽度×此倍数（或最小像素）视为同列。
const COL_TOL_MULT: f32 = 1.5;
const COL_TOL_MIN: f32 = 6.0;

/// 检测并排序竖排 region 簇。
///
/// 返回 `(竖排段落, 消费掩码)`——掩码与输入 `regions` 等长，`true` 表示该 region
/// 已被竖排消费，调用方应从后续横排排序中剔除。
pub fn order_vertical(regions: &[Region]) -> (Vec<String>, Vec<bool>) {
    if regions.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let page_w = regions.iter().map(|r| r.x_max).fold(0.0_f32, f32::max);
    // 候选：窄高条（高 > 2.5×宽，且宽 < 40% 页宽）
    let cand: Vec<(usize, &Region)> = regions
        .iter()
        .enumerate()
        .filter(|(_, r)| {
            let w = (r.x_max - r.x_min).max(1.0);
            let h = r.y_max - r.y_min;
            h > w * VERTICAL_RATIO && w < page_w * MAX_VERTICAL_WIDTH_FRAC
        })
        .collect();
    let mask_len = regions.len();
    if cand.is_empty() {
        return (Vec::new(), vec![false; mask_len]);
    }

    // 按 x 中心聚类成列（同列：x 中心接近当前列首条）
    let mut sorted = cand.clone();
    sorted.sort_by(|a, b| {
        a.1
            .center_x()
            .partial_cmp(&b.1.center_x())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut cols: Vec<Vec<(usize, &Region)>> = Vec::new();
    for item in sorted {
        let w = item.1.x_max - item.1.x_min;
        let tol = (w * COL_TOL_MULT).max(COL_TOL_MIN);
        if let Some(col) = cols.last_mut().filter(|c| {
            (c[0].1.center_x() - item.1.center_x()).abs() <= tol
        }) {
            col.push(item);
        } else {
            cols.push(vec![item]);
        }
    }

    // 列间按 x 降序（中文竖排从右到左）；列内按 y 排序（自上而下）
    cols.sort_by(|a, b| {
        b[0].1
            .center_x()
            .partial_cmp(&a[0].1.center_x())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut paras: Vec<String> = Vec::new();
    let mut mask = vec![false; mask_len];
    for mut col in cols {
        col.sort_by(|a, b| {
            a.1.y_min
                .partial_cmp(&b.1.y_min)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut text = String::new();
        for (i, r) in col {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(r.text.trim());
            mask[i] = true;
        }
        if !text.is_empty() {
            paras.push(text);
        }
    }
    (paras, mask)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::region::Region;

    fn vr(x_min: f32, y_min: f32, w: f32, h: f32, text: &str) -> Region {
        Region::new(x_min, x_min + w, y_min, y_min + h, text.to_string())
    }

    #[test]
    fn no_vertical_no_consumption() {
        // 普通横排宽条（高≈宽）→ 不消费任何 region
        let regions = vec![
            vr(10.0, 10.0, 200.0, 30.0, "横向一行"),
            vr(10.0, 50.0, 200.0, 30.0, "横向二行"),
        ];
        let (paras, mask) = order_vertical(&regions);
        assert!(paras.is_empty());
        assert!(mask.iter().all(|&m| !m));
    }

    #[test]
    fn single_column_top_to_bottom() {
        // 一列竖排：窄高条沿 y 排列 → 单段落、自上而下
        let regions = vec![
            vr(400.0, 300.0, 24.0, 90.0, "第一字"),
            vr(400.0, 200.0, 24.0, 90.0, "第二字"),
            vr(400.0, 100.0, 24.0, 90.0, "第三字"),
        ];
        let (paras, mask) = order_vertical(&regions);
        assert_eq!(paras.len(), 1);
        assert_eq!(paras[0], "第三字\n第二字\n第一字", "列内应自上而下");
        assert_eq!(mask, vec![true, true, true]);
    }

    #[test]
    fn columns_right_to_left() {
        // 两列竖排：列间应从右到左（x 大者先）
        let regions = vec![
            vr(300.0, 100.0, 24.0, 90.0, "左一"),
            vr(500.0, 100.0, 24.0, 90.0, "右一"),
            vr(300.0, 200.0, 24.0, 90.0, "左二"),
            vr(500.0, 200.0, 24.0, 90.0, "右二"),
        ];
        let (paras, mask) = order_vertical(&regions);
        assert_eq!(paras.len(), 2);
        assert_eq!(paras[0], "右一\n右二", "右侧列应优先");
        assert_eq!(paras[1], "左一\n左二");
        assert_eq!(mask, vec![true, true, true, true]);
    }

    #[test]
    fn wide_element_not_treated_as_vertical() {
        // 通栏标题（宽 > 40% 页宽）不应判竖排
        let regions = vec![vr(0.0, 0.0, 600.0, 60.0, "通栏标题")];
        let (paras, mask) = order_vertical(&regions);
        assert!(paras.is_empty());
        assert!(!mask[0]);
    }

    #[test]
    fn mixed_vertical_and_horizontal() {
        // 竖排 + 横排共存：竖排被消费，横排保留
        let regions = vec![
            vr(400.0, 100.0, 24.0, 90.0, "竖一"),
            vr(10.0, 10.0, 200.0, 30.0, "横一"),
        ];
        let (paras, mask) = order_vertical(&regions);
        assert_eq!(paras.len(), 1);
        assert!(mask[0], "竖排应被消费");
        assert!(!mask[1], "横排不应被消费");
    }
}
