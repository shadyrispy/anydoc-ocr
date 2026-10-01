//! merge_det_boxes 集成测试（主 crate 侧）。
//!
//! merge_det.rs 位于 third_party/oar-ocr-core（path 依赖、非 workspace 成员），
//! 其 #[cfg(test)] 单测无法经主 workspace 跑通；此处以公开 API 复刻同等用例。
//! 用例语义与 third_party/oar-ocr-core/src/processors/merge_det.rs 内嵌单测一致。

use oar_ocr::processors::{merge_det_boxes, BoundingBox, Point};

type Rect = (f32, f32, f32, f32);

fn box_xyxy(x0: f32, y0: f32, x1: f32, y1: f32) -> BoundingBox {
    BoundingBox::from_coords(x0, y0, x1, y1)
}

fn rect_of(b: &BoundingBox) -> Rect {
    (b.x_min(), b.y_min(), b.x_max(), b.y_max())
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
    assert_eq!(
        merged.len(),
        1,
        "fragments + title collapse into one line: {:?}",
        rects(&merged)
    );
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
