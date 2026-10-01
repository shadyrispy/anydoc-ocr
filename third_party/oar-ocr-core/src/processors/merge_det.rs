//! MinerU `merge_det_boxes`（model/ocr/geometry.py:149-171）的等价实现。
//!
//! MinerU OCR 子流程为 det → sorted → **merge_det_boxes** → crop → rec：
//! 同一视觉行的碎片框（det 对一行吐出的残条，rec 会产出乱码）与重复框
//! （同区域双检，rec 会产出重复文本）先按纵向重叠聚成行、行内按横向
//! 重叠合并，每个视觉行只裁图 rec 一次。本仓此前逐 det 框 rec，碎片与
//! 重复全部流入版面聚合，造成正文乱码拼接与整段重复（GJB 9001C 页 11/24）。
//!
//! 与 MinerU 的差异：增加表格/印章区域保护——MinerU 的表格文本由表格
//! 模型独立产出，不走通用 OCR 框；本仓表格 cell 填充依赖逐框粒度的
//! OCR 框与 cell 的交叠分配，合并成跨 cell 行框会破坏列归属，故与
//! Table/Seal 版面元素交叠的 det 框原样透传不合并。

use super::geometry::BoundingBox;

/// 行宽高比超过该值时行内再做横向重叠合并、保持 span 粒度（geometry.py:12）。
const LINE_WIDTH_TO_HEIGHT_RATIO_THRESHOLD: f32 = 4.0;

/// 纵向重叠占较矮框高度的比例超过该值视为同行（`merge_spans_to_line` 默认 0.6）。
const LINE_Y_OVERLAP_THRESHOLD: f32 = 0.6;

/// 与受保护版面元素（表格/印章）交叠超过该 IoA 的 det 框不参与合并。
const PROTECTED_IOA: f32 = 0.5;

type Rect = (f32, f32, f32, f32); // (x0, y0, x1, y1)

fn rect_of(b: &BoundingBox) -> Rect {
    (b.x_min(), b.y_min(), b.x_max(), b.y_max())
}

fn rect_from(r: Rect) -> BoundingBox {
    BoundingBox::from_coords(r.0, r.1, r.2, r.3)
}

/// geometry.py:32 `_is_overlaps_y_exceeds_threshold`。
fn overlaps_y_exceeds(a: Rect, b: Rect, threshold: f32) -> bool {
    let overlap = (a.3.min(b.3) - a.1.max(b.1)).max(0.0);
    let min_height = (a.3 - a.1).min(b.3 - b.1);
    min_height > 0.0 && (overlap / min_height) > threshold
}

/// geometry.py:174 `calculate_is_angle`：四点高度明显不一致的斜框。
fn is_angle(b: &BoundingBox) -> bool {
    if b.points.len() < 4 {
        return false;
    }
    let (p1, p2, p3, p4) = (&b.points[0], &b.points[1], &b.points[2], &b.points[3]);
    let height = ((p4.y - p1.y) + (p3.y - p2.y)) / 2.0;
    !(0.8 * height <= (p3.y - p1.y) && (p3.y - p1.y) <= 1.2 * height)
}

/// geometry.py:133 `merge_overlapping_spans`：行内横向有任何重叠
/// （prev.x_max ≥ cur.x_min）即并框，无比例阈值；0.8 比例版
/// `_is_overlaps_x_exceeds_threshold` 在 MinerU 中仅用于别处，不参与此合并。
fn merge_overlapping_spans(mut spans: Vec<Rect>) -> Vec<Rect> {
    spans.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut merged: Vec<Rect> = Vec::new();
    for span in spans {
        match merged.last_mut() {
            Some(last) if last.2 >= span.0 => {
                last.0 = last.0.min(span.0);
                last.1 = last.1.min(span.1);
                last.2 = last.2.max(span.2);
                last.3 = last.3.max(span.3);
            }
            _ => merged.push(span),
        }
    }
    merged
}

/// det 框是否与任一受保护元素交叠超过 `PROTECTED_IOA`（按 det 框自身面积）。
fn is_protected(b: &BoundingBox, protected: &[BoundingBox]) -> bool {
    if protected.is_empty() {
        return false;
    }
    let (bx0, by0, bx1, by1) = rect_of(b);
    let self_area = (bx1 - bx0) * (by1 - by0);
    if self_area <= 0.0 {
        return false;
    }
    protected.iter().any(|p| {
        let ix = (bx1.min(p.x_max()) - bx0.max(p.x_min())).max(0.0);
        let iy = (by1.min(p.y_max()) - by0.max(p.y_min())).max(0.0);
        ix * iy / self_area > PROTECTED_IOA
    })
}

/// 把 det 框按视觉行聚合合并；返回合并后的行框（poly 形式）。
///
/// - 斜框（`is_angle`）与受保护框（表格/印章）原样透传；
/// - 其余按 y0 排序、纵向重叠 > 0.6×min_height 聚行；
/// - 行宽 > 4×行高时行内横向有重叠的 span 才合并，保持 x 间隙条目
///   （如目次"编号 / 标题 / 页码"）的独立框粒度；否则整行并框。
pub fn merge_det_boxes(boxes: &[BoundingBox], protected: &[BoundingBox]) -> Vec<BoundingBox> {
    let mut horizontal: Vec<Rect> = Vec::with_capacity(boxes.len());
    let mut passthrough: Vec<BoundingBox> = Vec::new();
    for b in boxes {
        if is_protected(b, protected) || is_angle(b) {
            passthrough.push(b.clone());
        } else {
            horizontal.push(rect_of(b));
        }
    }

    // geometry.py:15 `merge_spans_to_line`：按 y0 排序后与当前行最后一个 span 比较纵向重叠。
    horizontal.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut lines: Vec<Vec<Rect>> = Vec::new();
    for span in horizontal {
        match lines.last_mut() {
            Some(line) if overlaps_y_exceeds(span, *line.last().expect("non-empty line"), LINE_Y_OVERLAP_THRESHOLD) => {
                line.push(span);
            }
            _ => lines.push(vec![span]),
        }
    }

    // geometry.py:149 `merge_det_boxes` 主体。
    let mut out: Vec<BoundingBox> = Vec::with_capacity(lines.len() + passthrough.len());
    for line in lines {
        let min_x = line.iter().map(|s| s.0).fold(f32::MAX, f32::min);
        let min_y = line.iter().map(|s| s.1).fold(f32::MAX, f32::min);
        let max_x = line.iter().map(|s| s.2).fold(f32::MIN, f32::max);
        let max_y = line.iter().map(|s| s.3).fold(f32::MIN, f32::max);
        if max_x - min_x > (max_y - min_y) * LINE_WIDTH_TO_HEIGHT_RATIO_THRESHOLD {
            out.extend(merge_overlapping_spans(line).into_iter().map(rect_from));
        } else {
            out.push(rect_from((min_x, min_y, max_x, max_y)));
        }
    }
    out.extend(passthrough);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::processors::geometry::Point;

    fn box_xyxy(x0: f32, y0: f32, x1: f32, y1: f32) -> BoundingBox {
        BoundingBox::from_coords(x0, y0, x1, y1)
    }

    fn rects(boxes: &[BoundingBox]) -> Vec<Rect> {
        boxes.iter().map(rect_of).collect()
    }

    /// 标题行碎片（残条 h=6 conf 低）与正常框聚成一行，只输出一个行框。
    #[test]
    fn fragment_boxes_merge_into_single_line() {
        // GJB 9001C 页 11 实测：标题框 + 3 个下半截碎片
        let boxes = vec![
            box_xyxy(97.0, 377.0, 250.0, 396.0),
            box_xyxy(97.0, 390.0, 250.0, 396.0),
            box_xyxy(125.0, 389.0, 252.0, 396.0),
            box_xyxy(97.0, 390.0, 250.0, 397.0),
        ];
        let merged = merge_det_boxes(&boxes, &[]);
        assert_eq!(merged.len(), 1, "fragments + title collapse into one line: {:?}", rects(&merged));
        assert_eq!(rect_of(&merged[0]), (97.0, 377.0, 252.0, 397.0));
    }

    /// 同区域双检框（相同矩形、不同 conf）合并为一。
    #[test]
    fn duplicate_boxes_collapse() {
        let boxes = vec![
            box_xyxy(125.0, 434.0, 677.0, 459.0),
            box_xyxy(125.0, 434.0, 677.0, 459.0),
        ];
        let merged = merge_det_boxes(&boxes, &[]);
        assert_eq!(merged.len(), 1);
    }

    /// 宽 > 4×行高的长行中 x 不重叠的条目保持独立 span（目次行不粘连）。
    #[test]
    fn wide_line_keeps_disjoint_spans() {
        let boxes = vec![
            box_xyxy(100.0, 400.0, 150.0, 420.0), // 编号
            box_xyxy(200.0, 401.0, 300.0, 419.0), // 标题
            box_xyxy(700.0, 400.0, 730.0, 420.0), // 页码
        ];
        let merged = merge_det_boxes(&boxes, &[]);
        assert_eq!(merged.len(), 3, "disjoint spans stay separate: {:?}", rects(&merged));
    }

    /// 宽 > 4×行高的长行中横向有重叠的 span（碎片+主体）合并。
    #[test]
    fn wide_line_merges_x_overlapping_spans() {
        let boxes = vec![
            box_xyxy(100.0, 400.0, 300.0, 420.0),
            box_xyxy(150.0, 401.0, 450.0, 419.0),
        ];
        let merged = merge_det_boxes(&boxes, &[]);
        assert_eq!(merged.len(), 1);
        assert_eq!(rect_of(&merged[0]), (100.0, 400.0, 450.0, 420.0));
    }

    /// 与表格元素交叠的 det 框透传，不参与行合并。
    #[test]
    fn table_overlapping_boxes_pass_through() {
        let table = box_xyxy(90.0, 100.0, 700.0, 500.0);
        let boxes = vec![
            box_xyxy(100.0, 150.0, 300.0, 170.0),
            box_xyxy(100.0, 151.0, 300.0, 170.0), // 表内碎片
        ];
        let merged = merge_det_boxes(&boxes, &[table]);
        assert_eq!(merged.len(), 2, "table boxes are not merged");
    }

    /// 斜框（四点高度不一致）透传。
    #[test]
    fn angled_boxes_pass_through() {
        let skewed = BoundingBox::new(vec![
            Point::new(100.0, 100.0),
            Point::new(300.0, 110.0),
            Point::new(300.0, 130.0),
            Point::new(100.0, 124.0),
        ]);
        let merged = merge_det_boxes(&[skewed.clone()], &[]);
        assert_eq!(merged.len(), 1);
        assert_eq!(rect_of(&merged[0]), rect_of(&skewed));
    }

    /// 相邻行（y 重叠不足）不聚行。
    #[test]
    fn adjacent_lines_stay_separate() {
        let boxes = vec![
            box_xyxy(100.0, 100.0, 300.0, 120.0),
            box_xyxy(100.0, 130.0, 300.0, 150.0), // y 重叠 0
        ];
        let merged = merge_det_boxes(&boxes, &[]);
        assert_eq!(merged.len(), 2);
    }
}
