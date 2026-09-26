//! MinerU PP-DocLayoutV2 block-level post-processing chain.
//!
//! Faithful port of `mineru/model/layout/pp_doclayout_v2_base.py`
//! (`_apply_paddlex_filter_boxes` at :666-720 and `_apply_layout_post_process`
//! at :646-663, with all helpers :140-643). Executed per page after the model's
//! score filter (`>= 0.45`), integer bbox clipping (`normalize_to_int_bbox`) and
//! reading-order lexsort (col6 asc, col7 desc) have been applied:
//!
//! 1. `paddlex_filter_boxes` (drop_inline_formula=False, MinerU's call argument)
//! 2. IoU-0.9 dedup (greedy by (-score, index), suppress `iou > 0.9`, restore order)
//! 3. nested formula merge (`overlap_ratio >= 0.7` → union bbox + max score)
//! 4. formula relabel (covered >= 0.7 by a non-formula/non-formula_number/non-reference
//!    box → `inline_formula`, else `display_formula`)
//! 5. header/footer boundary relabel (page-half fix → header anchor → footnote
//!    anchors → footer anchor with x-scope → number anchors at 30%/70% bands)
//! 6. internal visual caption filter (figure_title covered >= 0.8 inside
//!    image/chart/table/seal → drop)
//! 7. renumber indices 1..n
//!
//! Boxes keep label strings only; class ids are derived from `V2_LABELS` positions,
//! mirroring the Python dict that keeps `label` and `cls_id` in sync.

use crate::processors::BoundingBox;

/// `PP_DOCLAYOUT_V2_LABELS` (order = class id).
pub const V2_LABELS: [&str; 25] = [
    "abstract",
    "algorithm",
    "aside_text",
    "chart",
    "content",
    "display_formula",
    "doc_title",
    "figure_title",
    "footer",
    "footer_image",
    "footnote",
    "formula_number",
    "header",
    "header_image",
    "image",
    "inline_formula",
    "number",
    "paragraph_title",
    "reference",
    "reference_content",
    "seal",
    "table",
    "text",
    "vertical_text",
    "vision_footnote",
];

// Class ids (indices into V2_LABELS).
const ASIDE_TEXT: usize = 2;
const CHART: usize = 3;
const DISPLAY_FORMULA: usize = 5;
const FIGURE_TITLE: usize = 7;
const FOOTER: usize = 8;
const FOOTER_IMAGE: usize = 9;
const FOOTNOTE: usize = 10;
const FORMULA_NUMBER: usize = 11;
const HEADER: usize = 12;
const HEADER_IMAGE: usize = 13;
const IMAGE: usize = 14;
const INLINE_FORMULA: usize = 15;
const NUMBER: usize = 16;
const REFERENCE: usize = 18;
const SEAL: usize = 20;
const TABLE: usize = 21;

fn label_id(label: &str) -> Option<usize> {
    V2_LABELS.iter().position(|l| *l == label)
}

/// A layout box in the shape Python's postprocessor consumes
/// (`cls_id`/`label`/`score`/`bbox`/`index`).
#[derive(Debug, Clone)]
pub struct MineruBox {
    pub bbox: BoundingBox,
    pub label: String,
    pub score: f32,
}

impl MineruBox {
    pub fn new(bbox: BoundingBox, label: impl Into<String>, score: f32) -> Self {
        Self {
            bbox,
            label: label.into(),
            score,
        }
    }

    fn cls(&self) -> Option<usize> {
        label_id(&self.label)
    }
}

// ---------------------------------------------------------------- geometry

fn area(b: &BoundingBox) -> f32 {
    (b.x_max() - b.x_min()).max(0.0) * (b.y_max() - b.y_min()).max(0.0)
}

fn intersection_area(a: &BoundingBox, b: &BoundingBox) -> f32 {
    // Raw arithmetic mirroring Python `max(0, xmax - xmin) * max(0, ymax - ymin)`.
    // Must NOT round-trip through `BoundingBox::from_coords` here: the corner
    // accessors take min/max over the point set, so an empty intersection
    // (xmax < xmin) would be silently flipped into a bogus positive box.
    let xmin = a.x_min().max(b.x_min());
    let ymin = a.y_min().max(b.y_min());
    let xmax = a.x_max().min(b.x_max());
    let ymax = a.y_max().min(b.y_max());
    (xmax - xmin).max(0.0) * (ymax - ymin).max(0.0)
}

/// `_calculate_overlap_ratio`: intersection / min(area1, area2).
fn overlap_ratio(a: &BoundingBox, b: &BoundingBox) -> f32 {
    let ref_area = area(a).min(area(b));
    if ref_area <= 0.0 {
        return 0.0;
    }
    intersection_area(a, b) / ref_area
}

/// `_calculate_iou`.
fn iou(a: &BoundingBox, b: &BoundingBox) -> f32 {
    let inter = intersection_area(a, b);
    let union = area(a) + area(b) - inter;
    if union <= 0.0 {
        return 0.0;
    }
    inter / union
}

/// `_calculate_cover_ratio`: intersection / area(box1).
fn cover_ratio(inner: &BoundingBox, outer: &BoundingBox) -> f32 {
    let a = area(inner);
    if a <= 0.0 {
        return 0.0;
    }
    intersection_area(inner, outer) / a
}

/// `_calculate_x_overlap_ratio`: horizontal overlap / min(width).
fn x_overlap_ratio(a: &BoundingBox, b: &BoundingBox) -> f32 {
    let w1 = (a.x_max() - a.x_min()).max(0.0);
    let w2 = (b.x_max() - b.x_min()).max(0.0);
    let ref_w = w1.min(w2);
    if ref_w <= 0.0 {
        return 0.0;
    }
    (a.x_max().min(b.x_max()) - a.x_min().max(b.x_min())).max(0.0) / ref_w
}

/// `_calculate_x_cover_ratio`: anchor's horizontal coverage of candidate.
fn x_cover_ratio(anchor: &BoundingBox, candidate: &BoundingBox) -> f32 {
    let cw = (candidate.x_max() - candidate.x_min()).max(0.0);
    if cw <= 0.0 {
        return 0.0;
    }
    (anchor.x_max().min(candidate.x_max()) - anchor.x_min().max(candidate.x_min())).max(0.0) / cw
}

/// `_union_bbox`: floor(min) / ceil(max) — coordinates are already integers here.
fn union_bbox(a: &BoundingBox, b: &BoundingBox) -> BoundingBox {
    BoundingBox::from_coords(
        a.x_min().min(b.x_min()),
        a.y_min().min(b.y_min()),
        a.x_max().max(b.x_max()),
        a.y_max().max(b.y_max()),
    )
}

/// Python `round(float(score), 4)`.
pub fn round4(v: f32) -> f32 {
    (v * 10000.0).round() / 10000.0
}

fn set_label(mb: &mut MineruBox, cls: usize) {
    mb.label = V2_LABELS[cls].to_string();
}

fn is_formula(mb: &MineruBox) -> bool {
    matches!(mb.cls(), Some(DISPLAY_FORMULA) | Some(INLINE_FORMULA))
}

fn is_inline_formula(mb: &MineruBox) -> bool {
    mb.cls() == Some(INLINE_FORMULA)
}

fn is_reference(mb: &MineruBox) -> bool {
    mb.cls() == Some(REFERENCE)
}

fn is_formula_number(mb: &MineruBox) -> bool {
    mb.cls() == Some(FORMULA_NUMBER)
}

// ---------------------------------------------------------------- chain

/// `_apply_paddlex_filter_boxes(drop_inline_formula=False)`.
fn paddlex_filter_boxes(boxes: &[MineruBox]) -> Vec<MineruBox> {
    let kept: Vec<MineruBox> = boxes
        .iter()
        .filter(|b| !is_reference(b))
        .cloned()
        .collect();
    let n = kept.len();
    let mut dropped = vec![false; n];

    for i in 0..n {
        if dropped[i] {
            continue;
        }
        let bi = &kept[i];
        let width = bi.bbox.x_max() - bi.bbox.x_min();
        let height = bi.bbox.y_max() - bi.bbox.y_min();
        if (width < 6.0 || height < 6.0) && !is_inline_formula(bi) {
            dropped[i] = true;
            continue;
        }
        for j in (i + 1)..n {
            if dropped[i] || dropped[j] {
                continue;
            }
            if is_inline_formula(&kept[i]) || is_inline_formula(&kept[j]) {
                continue;
            }
            let ratio = overlap_ratio(&kept[i].bbox, &kept[j].bbox);
            if ratio > 0.7 {
                // Python: `labels & {image,table,seal,chart} and len(labels) > 1`
                // then keep both iff `"table" not in labels or
                // labels <= {table,image,seal,chart}`.
                let vis = |l: &str| matches!(l, "image" | "table" | "seal" | "chart");
                let li = kept[i].label.as_str();
                let lj = kept[j].label.as_str();
                if (vis(li) || vis(lj)) && li != lj {
                    let has_table = li == "table" || lj == "table";
                    if !has_table || (vis(li) && vis(lj)) {
                        continue;
                    }
                }
                if area(&kept[i].bbox) >= area(&kept[j].bbox) {
                    dropped[j] = true;
                } else {
                    dropped[i] = true;
                    break;
                }
            }
        }
    }
    let mut out = Vec::with_capacity(n);
    for (i, b) in kept.into_iter().enumerate() {
        if !dropped[i] {
            out.push(b);
        }
    }
    out
}

/// `_deduplicate_boxes_by_iou(threshold=0.9)`. Index = position in `boxes`.
fn deduplicate_boxes_by_iou(boxes: Vec<MineruBox>, iou_threshold: f32) -> Vec<MineruBox> {
    if boxes.len() <= 1 {
        return boxes;
    }
    let mut order: Vec<usize> = (0..boxes.len()).collect();
    order.sort_by(|&a, &b| {
        boxes[b]
            .score
            .total_cmp(&boxes[a].score)
            .then_with(|| a.cmp(&b))
    });
    let mut suppressed = vec![false; boxes.len()];
    for pos in 0..order.len() {
        let current = order[pos];
        if suppressed[current] {
            continue;
        }
        for &other in &order[pos + 1..] {
            if !suppressed[other]
                && iou(&boxes[current].bbox, &boxes[other].bbox) > iou_threshold
            {
                suppressed[other] = true;
            }
        }
    }
    boxes
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !suppressed[*i])
        .map(|(_, b)| b)
        .collect()
}

/// `_merge_nested_formula_boxes(overlap_threshold=0.7)`.
fn merge_nested_formula_boxes(mut boxes: Vec<MineruBox>, overlap_threshold: f32) -> Vec<MineruBox> {
    if boxes.len() <= 1 {
        return boxes;
    }
    loop {
        let mut changed = false;
        let formula_indexes: Vec<usize> = (0..boxes.len())
            .filter(|&i| is_formula(&boxes[i]))
            .collect();
        'outer: for (pos, &left_index) in formula_indexes.iter().enumerate() {
            for &right_index in &formula_indexes[pos + 1..] {
                let ratio = overlap_ratio(&boxes[left_index].bbox, &boxes[right_index].bbox);
                if ratio < overlap_threshold {
                    continue;
                }
                let left_area = area(&boxes[left_index].bbox);
                let right_area = area(&boxes[right_index].bbox);
                let (keep, drop) = if left_area > right_area {
                    (left_index, right_index)
                } else if right_area > left_area {
                    (right_index, left_index)
                } else if boxes[left_index].score >= boxes[right_index].score {
                    (left_index, right_index)
                } else {
                    (right_index, left_index)
                };
                let merged_bbox = union_bbox(&boxes[keep].bbox, &boxes[drop].bbox);
                let merged_score =
                    round4(boxes[keep].score.max(boxes[drop].score));
                boxes[keep].bbox = merged_bbox;
                boxes[keep].score = merged_score;
                boxes.remove(drop);
                changed = true;
                break 'outer;
            }
        }
        if !changed {
            return boxes;
        }
    }
}

/// `_relabel_formula_boxes(overlap_threshold=0.7)`.
fn relabel_formula_boxes(mut boxes: Vec<MineruBox>, overlap_threshold: f32) -> Vec<MineruBox> {
    let parent_candidates: Vec<usize> = (0..boxes.len())
        .filter(|&i| !is_formula(&boxes[i]) && !is_formula_number(&boxes[i]) && !is_reference(&boxes[i]))
        .collect();
    for i in 0..boxes.len() {
        if !is_formula(&boxes[i]) {
            continue;
        }
        let mut target = DISPLAY_FORMULA;
        for &pi in &parent_candidates {
            if cover_ratio(&boxes[i].bbox, &boxes[pi].bbox) >= overlap_threshold {
                target = INLINE_FORMULA;
                break;
            }
        }
        set_label(&mut boxes[i], target);
    }
    boxes
}

/// `_reclassify_header_footer_by_page_half`.
fn reclassify_header_footer_by_page_half(boxes: &mut [MineruBox], image_size: (f32, f32)) {
    let (page_height, _) = image_size;
    if page_height <= 0.0 {
        return;
    }
    let page_middle = page_height * 0.5;
    for mb in boxes.iter_mut() {
        let Some(cls) = mb.cls() else { continue };
        let y_mid = (mb.bbox.y_min() + mb.bbox.y_max()) / 2.0;
        let target = if y_mid < page_middle {
            match cls {
                FOOTER => Some(HEADER),
                FOOTER_IMAGE => Some(HEADER_IMAGE),
                _ => None,
            }
        } else {
            match cls {
                HEADER => Some(FOOTER),
                HEADER_IMAGE => Some(FOOTER_IMAGE),
                _ => None,
            }
        };
        if let Some(target) = target {
            set_label(mb, target);
        }
    }
}

/// `HEADER_FOOTER_BOUNDARY_EXEMPT_LABELS` = {aside_text, footnote, number}.
fn is_header_footer_boundary_candidate(mb: &MineruBox, anchor_labels: &[usize]) -> bool {
    let Some(cls) = mb.cls() else {
        return false;
    };
    if matches!(cls, ASIDE_TEXT | FOOTNOTE | NUMBER) {
        return false;
    }
    !anchor_labels.contains(&cls)
}

/// `PAGE_REGION_LABELS` check for the footnote relabel candidate filter.
fn is_footnote_relabel_candidate(mb: &MineruBox) -> bool {
    match mb.cls() {
        Some(c) => !matches!(
            c,
            HEADER | HEADER_IMAGE | FOOTER | FOOTER_IMAGE | FOOTNOTE | NUMBER | ASIDE_TEXT
        ),
        None => true,
    }
}

/// `_is_covered_by_footnote`: candidate below footnote top and covered 0.7 laterally.
fn is_covered_by_footnote(footnote: &MineruBox, candidate: &MineruBox) -> bool {
    if candidate.bbox.y_min() < footnote.bbox.y_min() {
        return false;
    }
    x_cover_ratio(&footnote.bbox, &candidate.bbox) >= 0.7
}

/// `_is_footer_x_scope`: anchor spans >= 70% page width, or 0.3 lateral overlap.
fn is_footer_x_scope(
    anchor: &MineruBox,
    candidate: &MineruBox,
    image_size: (f32, f32),
) -> bool {
    let (_, page_width) = image_size;
    let anchor_width = anchor.bbox.x_max() - anchor.bbox.x_min();
    if page_width > 0.0 && anchor_width / page_width >= 0.7 {
        return true;
    }
    x_overlap_ratio(&anchor.bbox, &candidate.bbox) >= 0.3
}

/// `_relabel_header_footer_boundary_blocks(image_size)`. Input boxes are in
/// index order (Python sorts by `box["index"]`, which equals list order here).
fn relabel_header_footer_boundary(
    mut boxes: Vec<MineruBox>,
    image_size: (f32, f32),
) -> Vec<MineruBox> {
    if boxes.len() <= 1 {
        return boxes;
    }
    let header_labels = [HEADER, HEADER_IMAGE];
    let footer_labels = [FOOTER, FOOTER_IMAGE];

    reclassify_header_footer_by_page_half(&mut boxes, image_size);

    // boundary_anchor_ids: boxes that are header/footer *after* the half fix.
    let boundary_anchor_ids: Vec<usize> = (0..boxes.len())
        .filter(|&i| {
            boxes[i]
                .cls()
                .is_some_and(|c| header_labels.contains(&c) || footer_labels.contains(&c))
        })
        .collect();

    // header_anchor: max (bbox.y_max, index) → larger index wins ties.
    let header_anchor = (0..boxes.len())
        .filter(|&i| boxes[i].cls().is_some_and(|c| header_labels.contains(&c)))
        .max_by(|&a, &b| {
            boxes[a]
                .bbox
                .y_max()
                .total_cmp(&boxes[b].bbox.y_max())
                .then_with(|| a.cmp(&b))
        });
    // footer_anchor: min (bbox.y_min, index) → smaller index wins ties.
    let footer_anchor = (0..boxes.len())
        .filter(|&i| boxes[i].cls().is_some_and(|c| footer_labels.contains(&c)))
        .min_by(|&a, &b| {
            boxes[a]
                .bbox
                .y_min()
                .total_cmp(&boxes[b].bbox.y_min())
                .then_with(|| a.cmp(&b))
        });

    if let Some(ha) = header_anchor {
        let boundary = boxes[ha].bbox.y_max();
        for i in 0..boxes.len() {
            if !is_header_footer_boundary_candidate(&boxes[i], &header_labels) {
                continue;
            }
            if boxes[i].bbox.y_max() <= boundary {
                set_label(&mut boxes[i], HEADER);
            }
        }
    }

    let footnote_anchors: Vec<usize> = (0..boxes.len())
        .filter(|&i| boxes[i].cls() == Some(FOOTNOTE))
        .collect();
    if !footnote_anchors.is_empty() {
        for i in 0..boxes.len() {
            if !is_footnote_relabel_candidate(&boxes[i]) {
                continue;
            }
            for &fa in &footnote_anchors {
                if is_covered_by_footnote(&boxes[fa], &boxes[i]) {
                    set_label(&mut boxes[i], FOOTNOTE);
                    break;
                }
            }
        }
    }

    if let Some(fa) = footer_anchor {
        let boundary = boxes[fa].bbox.y_min();
        for i in 0..boxes.len() {
            if !is_header_footer_boundary_candidate(&boxes[i], &footer_labels) {
                continue;
            }
            if boxes[i].bbox.y_min() >= boundary && is_footer_x_scope(&boxes[fa], &boxes[i], image_size)
            {
                set_label(&mut boxes[i], FOOTER);
            }
        }
    }

    let (page_height, _) = image_size;
    if page_height <= 0.0 {
        return boxes;
    }
    let top_boundary = page_height * 0.3;
    let bottom_boundary = page_height * 0.7;
    let mut top_numbers: Vec<usize> = Vec::new();
    let mut bottom_numbers: Vec<usize> = Vec::new();
    for i in 0..boxes.len() {
        if boxes[i].cls() != Some(NUMBER) {
            continue;
        }
        let y_mid = (boxes[i].bbox.y_min() + boxes[i].bbox.y_max()) / 2.0;
        if y_mid <= top_boundary {
            top_numbers.push(i);
        } else if y_mid >= bottom_boundary {
            bottom_numbers.push(i);
        }
    }
    let top_number_anchor = top_numbers.into_iter().max_by(|&a, &b| {
        boxes[a]
            .bbox
            .y_max()
            .total_cmp(&boxes[b].bbox.y_max())
            .then_with(|| a.cmp(&b))
    });
    let bottom_number_anchor = bottom_numbers.into_iter().min_by(|&a, &b| {
        boxes[a]
            .bbox
            .y_min()
            .total_cmp(&boxes[b].bbox.y_min())
            .then_with(|| a.cmp(&b))
    });

    if let Some(tn) = top_number_anchor {
        let boundary = boxes[tn].bbox.y_min();
        for i in 0..boxes.len() {
            if boundary_anchor_ids.contains(&i) {
                continue;
            }
            if !is_header_footer_boundary_candidate(&boxes[i], &[]) {
                continue;
            }
            if boxes[i].bbox.y_max() <= boundary {
                set_label(&mut boxes[i], HEADER);
            }
        }
    }
    if let Some(bn) = bottom_number_anchor {
        let boundary = boxes[bn].bbox.y_max();
        for i in 0..boxes.len() {
            if boundary_anchor_ids.contains(&i) {
                continue;
            }
            if !is_header_footer_boundary_candidate(&boxes[i], &[]) {
                continue;
            }
            if boxes[i].bbox.y_min() >= boundary {
                set_label(&mut boxes[i], FOOTER);
            }
        }
    }

    boxes
}

/// `_filter_internal_visual_caption_boxes(cover_threshold=0.8)`.
fn filter_internal_visual_caption_boxes(
    boxes: Vec<MineruBox>,
    cover_threshold: f32,
) -> Vec<MineruBox> {
    let visual_boxes: Vec<BoundingBox> = boxes
        .iter()
        .filter(|b| {
            b.cls().is_some_and(|c| {
                matches!(c, IMAGE | CHART | TABLE | SEAL)
            })
        })
        .map(|b| b.bbox.clone())
        .collect();
    if visual_boxes.is_empty() {
        return boxes;
    }
    boxes
        .into_iter()
        .filter(|b| {
            if b.cls() != Some(FIGURE_TITLE) {
                return true;
            }
            let cx = (b.bbox.x_min() + b.bbox.x_max()) / 2.0;
            let cy = (b.bbox.y_min() + b.bbox.y_max()) / 2.0;
            for v in &visual_boxes {
                if v.x_min() <= cx
                    && cx <= v.x_max()
                    && v.y_min() <= cy
                    && cy <= v.y_max()
                    && cover_ratio(&b.bbox, v) >= cover_threshold
                {
                    return false;
                }
            }
            true
        })
        .collect()
}

/// `_apply_layout_post_process(image_size)`: full MinerU block chain.
/// Input order must be the model reading order (lexsort already applied);
/// output preserves that order with indices renumbered implicitly.
pub fn mineru_layout_post_process(
    boxes: Vec<MineruBox>,
    image_size: (f32, f32),
) -> Vec<MineruBox> {
    let boxes = paddlex_filter_boxes(&boxes);
    let boxes = deduplicate_boxes_by_iou(boxes, 0.9);
    let boxes = merge_nested_formula_boxes(boxes, 0.7);
    let boxes = relabel_formula_boxes(boxes, 0.7);
    let boxes = relabel_header_footer_boundary(boxes, image_size);
    let boxes = filter_internal_visual_caption_boxes(boxes, 0.8);
    // `_renumber_indices` is implicit: list order is the index order.
    boxes
}

/// `normalize_to_int_bbox` (docvortex.geometry): floor/ceil + clamp to image,
/// drop degenerate boxes.
pub fn normalize_to_int_bbox(
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    width: f32,
    height: f32,
) -> Option<BoundingBox> {
    let mut xmin = x1.floor();
    let mut ymin = y1.floor();
    let mut xmax = x2.ceil();
    let mut ymax = y2.ceil();
    let w = width.max(0.0).floor();
    let h = height.max(0.0).floor();
    xmin = xmin.clamp(0.0, w);
    ymin = ymin.clamp(0.0, h);
    xmax = xmax.clamp(0.0, w);
    ymax = ymax.clamp(0.0, h);
    if xmax <= xmin || ymax <= ymin {
        return None;
    }
    Some(BoundingBox::from_coords(xmin, ymin, xmax, ymax))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(x1: f32, y1: f32, x2: f32, y2: f32) -> BoundingBox {
        BoundingBox::from_coords(x1, y1, x2, y2)
    }

    fn bx(label: &str, x1: f32, y1: f32, x2: f32, y2: f32, score: f32) -> MineruBox {
        MineruBox::new(b(x1, y1, x2, y2), label, score)
    }

    #[test]
    fn reference_boxes_are_dropped_first() {
        let boxes = vec![
            bx("reference", 0.0, 0.0, 100.0, 50.0, 0.9),
            bx("text", 10.0, 10.0, 90.0, 40.0, 0.8),
        ];
        let kept = paddlex_filter_boxes(&boxes);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].label, "text");
    }

    #[test]
    fn tiny_boxes_dropped_but_inline_formula_kept() {
        let boxes = vec![
            bx("text", 0.0, 0.0, 4.0, 100.0, 0.9),
            bx("inline_formula", 50.0, 50.0, 54.0, 55.0, 0.9),
        ];
        let kept = paddlex_filter_boxes(&boxes);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].label, "inline_formula");
    }

    #[test]
    fn two_visual_bodies_both_survive() {
        let boxes = vec![
            bx("image", 0.0, 0.0, 100.0, 100.0, 0.9),
            bx("chart", 5.0, 5.0, 95.0, 95.0, 0.8),
        ];
        let kept = paddlex_filter_boxes(&boxes);
        assert_eq!(kept.len(), 2);
    }

    /// Python: labels {image, text} → "table" not in labels → keep both.
    #[test]
    fn visual_body_vs_text_both_survive() {
        let boxes = vec![
            bx("image", 0.0, 0.0, 100.0, 100.0, 0.9),
            bx("text", 5.0, 5.0, 95.0, 95.0, 0.8),
        ];
        let kept = paddlex_filter_boxes(&boxes);
        assert_eq!(kept.len(), 2);
    }

    /// Python: labels {table, text} → table in labels and not ⊆ visual set →
    /// smaller box dropped.
    #[test]
    fn table_vs_text_drops_smaller() {
        let boxes = vec![
            bx("table", 0.0, 0.0, 100.0, 100.0, 0.9),
            bx("text", 10.0, 10.0, 90.0, 90.0, 0.8),
        ];
        let kept = paddlex_filter_boxes(&boxes);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].label, "table");
    }

    /// Empty-intersection geometry guard: disjoint boxes must not register overlap.
    #[test]
    fn disjoint_boxes_never_drop() {
        let boxes = vec![
            bx("header_image", 981.0, 65.0, 1412.0, 244.0, 0.74),
            bx("text", 202.0, 451.0, 359.0, 504.0, 0.9),
        ];
        let kept = paddlex_filter_boxes(&boxes);
        assert_eq!(kept.len(), 2);
        assert!(overlap_ratio(&boxes[0].bbox, &boxes[1].bbox) < 0.7);
    }

    #[test]
    fn overlap_drops_smaller_text_box() {
        let boxes = vec![
            bx("text", 0.0, 0.0, 100.0, 100.0, 0.9),
            bx("text", 10.0, 10.0, 90.0, 90.0, 0.8),
        ];
        let kept = paddlex_filter_boxes(&boxes);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].score, 0.9);
    }

    #[test]
    fn iou_dedup_keeps_first_order_position() {
        let boxes = vec![
            bx("text", 0.0, 0.0, 100.0, 100.0, 0.5),
            bx("text", 1.0, 1.0, 100.0, 100.0, 0.9), // IoU ~0.98 > 0.9 → suppressed by idx1
        ];
        let kept = deduplicate_boxes_by_iou(boxes, 0.9);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].score, 0.9);
    }

    #[test]
    fn formula_merge_unions_and_keeps_max_score() {
        let boxes = vec![
            bx("display_formula", 0.0, 0.0, 40.0, 40.0, 0.6),
            bx("display_formula", 10.0, 0.0, 60.0, 40.0, 0.9),
            bx("text", 0.0, 200.0, 60.0, 300.0, 0.95),
        ];
        let merged = merge_nested_formula_boxes(boxes, 0.7);
        assert_eq!(merged.len(), 2);
        let f = &merged[0];
        assert_eq!(f.score, 0.9);
        assert_eq!(f.bbox.x_max(), 60.0);
        assert_eq!(f.bbox.x_min(), 0.0);
    }

    #[test]
    fn formula_relabel_nested_to_inline() {
        let boxes = vec![
            bx("display_formula", 10.0, 10.0, 20.0, 20.0, 0.9),
            bx("text", 0.0, 0.0, 100.0, 100.0, 0.9),
        ];
        let relabeled = relabel_formula_boxes(boxes, 0.7);
        assert_eq!(relabeled[0].label, "inline_formula");
    }

    #[test]
    fn header_footer_half_reclassification() {
        let boxes = vec![
            bx("footer", 0.0, 10.0, 100.0, 30.0, 0.9), // mid y=20 < 500*0.5 → header
            bx("header", 0.0, 900.0, 100.0, 940.0, 0.9), // → footer
        ];
        let relabeled = relabel_header_footer_boundary(boxes, (1000.0, 100.0));
        assert_eq!(relabeled[0].label, "header");
        assert_eq!(relabeled[1].label, "footer");
    }

    #[test]
    fn header_boundary_relabels_upper_text() {
        let boxes = vec![
            bx("header", 0.0, 10.0, 100.0, 40.0, 0.9),
            bx("text", 0.0, 10.0, 100.0, 35.0, 0.8), // y_max 35 <= 40 → header
            bx("text", 0.0, 500.0, 100.0, 700.0, 0.9),
        ];
        let relabeled = relabel_header_footer_boundary(boxes, (1000.0, 100.0));
        assert_eq!(relabeled[1].label, "header");
        assert_eq!(relabeled[2].label, "text");
    }

    #[test]
    fn internal_caption_filtered() {
        let boxes = vec![
            bx("image", 0.0, 0.0, 100.0, 100.0, 0.9),
            bx("figure_title", 10.0, 10.0, 60.0, 30.0, 0.9), // center inside, covered
            bx("figure_title", 0.0, 120.0, 100.0, 140.0, 0.9), // outside image → kept
        ];
        let kept = filter_internal_visual_caption_boxes(boxes, 0.8);
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn normalize_int_bbox_semantics() {
        assert_eq!(
            normalize_to_int_bbox(0.2, 0.8, 10.7, 11.2, 100.0, 100.0).map(|bb| [
                bb.x_min(),
                bb.y_min(),
                bb.x_max(),
                bb.y_max()
            ]),
            Some([0.0, 0.0, 11.0, 12.0])
        );
        // degenerate after clamp → None
        assert!(normalize_to_int_bbox(200.0, 0.0, 300.0, 10.0, 100.0, 100.0).is_none());
    }
}
