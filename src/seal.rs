//! 印章识别接线（默认开启，`ANYDOC_NO_SEAL_OCR` 关闭；#10b 前为 `ANYDOC_SEAL_OCR`
//! 存在即开启）——纯函数内核。
//!
//! 实测结论（本仓 vendored oar-ocr，2026-09 取证）：版面模型（PP-DocLayout-S/M/
//! V2/V3）**会**输出 `LayoutElementType::Seal`，但 `with_seal_text_detection`
//! 只做 DB 文字**检测**（拿多边形框），不把识别文本写回 `LayoutElement.text`；
//! `stitching` 又把 Seal 排除在普通 OCR 匹配之外（`EXCLUDED_FROM_OCR_LABELS`）
//! 并把重叠文本区域标记为已用——即印章区域内的文字在现网路径**整体丢失**。
//! 且该适配器一旦挂上，`precompute_overall_ocr_across_pages` 直接早退，
//! 跨页批量 OCR 优化被禁用。
//!
//! 故本票不改 vendored 管线，改走**我们自己的后处理层**（`ocr_post`）：
//! 版面已给出 Seal bbox → 从页图裁出 → 印章专用检测器出行框 → min-area-rect
//! 摆正 → 复用 tier 的 rec 识别 → 文本写回 `LayoutElement.text` → `gfm_adapter`
//! 输出 `【印章】…` 行。开关状态进引擎缓存键（`ocr_engine::EngineKey`），且
//! `seal_pass` 在页面无 Seal 元素时早退——**不含章的文档不建额外 session**。
//!
//! ## 已知边界：环排（弧形）文字暂不识别
//!
//! 印章检测对**章顶环排公司名**输出沿弧走行的多顶点多边形（实测 90 顶点），
//! min-area-rect 摆正会把弧压成斜矩形，rec 只认出一两个残缺字（实测
//! 「北京测试科技有限公司」→「时技有限」）。残缺字写进输出比不写更坏，故本票
//! 用 [`is_curved_band`] 显式**跳过**这类行，只识别近似直的行（章底「专用章」等）。
//! 弧行矫正（中线/极坐标重采样）留作后续，取证与设计结论见 `BACKLOG.md` #5a。
//!
//! 本模块只含**无模型依赖**的几何/文本内核，可完整单测。

use oar_ocr::processors::{BoundingBox, Point};

/// 印章块重叠去重阈值（IoU）：版面模型对同一枚章常出 2 个嵌套框
/// （实测外圈 0.979 + 内圈 0.626），不去重会重复识别、重复输出。
pub const SEAL_IOU_DEDUPE: f32 = 0.5;

/// 印章行内去重后拼成一行的分隔符（章内文字通常同属一句）。
pub const SEAL_LINE_SEP: &str = " ";

/// 前缀标记（GFM 输出用，见 `gfm_adapter`）。
pub const SEAL_TAG: &str = "【印章】";

/// 弧行判定阈值（MinerU `seal_crop.py::get_poly_rect_crop` 的 0.7 同款口径：
/// polygon 与 min-area-rect 面积比低于此 ⇒ 沿弧走行的带结构，不是直行框）。
pub const ARC_QUAD_COVER: f32 = 0.7;

/// 按给定顺序贪心去重：保留先出现的框，丢弃与**已保留**框 IoU 超阈者。
///
/// 返回保留项的下标（升序）。IoU 由 [`BoundingBox::iou`] 计算（多边形外接矩形
/// 口径，与上游表格/版面去重一致）。退化框（面积 0）互相 IoU=0 → 都保留，
/// 交由调用方的裁剪失败降级兜底。
pub fn dedupe_overlapping(boxes: &[BoundingBox], iou_threshold: f32) -> Vec<usize> {
    let mut kept: Vec<usize> = Vec::new();
    for (i, b) in boxes.iter().enumerate() {
        if kept
            .iter()
            .any(|&j| boxes[j].iou(b) > iou_threshold)
        {
            continue;
        }
        kept.push(i);
    }
    kept
}

/// 行框是否**沿弧走行**（环排文字）：是 → 本票不识别（见模块文档「已知边界」）。
///
/// 判据即 MinerU 分流口径 [`ARC_QUAD_COVER`]：多边形填满其 min-area-rect 的程度。
/// 实测（fixture `tests/samples/seal_scan.pdf`）章底直排「专用章」cover ≈ 0.90，
/// 章顶 126° 环排 cover ≈ 0.50，两个判定各自稳定。
pub fn is_curved_band(points: &[Point]) -> bool {
    quad_cover_ratio(points) < ARC_QUAD_COVER
}

/// polygon 面积 / min-area-rect 面积（MinerU IoU 分流的覆盖比口径）。
pub fn quad_cover_ratio(points: &[Point]) -> f32 {
    let poly = BoundingBox::new(points.to_vec()).area();
    let rect = BoundingBox::get_min_area_rect_from_points(points);
    let ra = rect.width * rect.height;
    if ra <= 1.0 {
        return 0.0;
    }
    (poly / ra).clamp(0.0, 1.0)
}

/// 把印章内的识别行规整为单行文本（**不含** `【印章】` 前缀——前缀由
/// `gfm_adapter` 输出层统一添加，数据结构里只存原始识别结果）。
///
/// - 逐行 trim、丢空行；
/// - 精确去重（同一行识别两次只保留一次——嵌套框/环形文字重复检测的实测形态）；
/// - 全部为空 → `None`（不写回、不输出）。
pub fn join_seal_lines(lines: &[String]) -> Option<String> {
    let mut seen: Vec<&str> = Vec::new();
    for l in lines {
        let t = l.trim();
        if !t.is_empty() && !seen.contains(&t) {
            seen.push(t);
        }
    }
    if seen.is_empty() {
        return None;
    }
    Some(seen.join(SEAL_LINE_SEP))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    fn bb(x0: f32, y0: f32, x1: f32, y1: f32) -> BoundingBox {
        BoundingBox::from_coords(x0, y0, x1, y1)
    }

    /// 嵌套框（实测形态：外圈 + 内圈同一枚章）→ 只留先出现的大框。
    #[test]
    fn nested_seal_boxes_dedupe_to_one() {
        let boxes = vec![bb(100., 100., 300., 300.), bb(120., 120., 280., 280.)];
        assert_eq!(dedupe_overlapping(&boxes, SEAL_IOU_DEDUPE), vec![0]);
    }

    /// 分离的两枚章 → 都保留。
    #[test]
    fn distant_seals_both_kept() {
        let boxes = vec![bb(0., 0., 50., 50.), bb(200., 200., 260., 260.)];
        assert_eq!(dedupe_overlapping(&boxes, SEAL_IOU_DEDUPE), vec![0, 1]);
    }

    /// 半重叠（IoU 落在阈值两侧）：小比例重叠保留，重叠过半丢弃。
    #[test]
    fn partial_overlap_uses_threshold() {
        // 100x100 与右移 20 → 交叠 80x100，IoU = 8000/12000 ≈ 0.667 > 0.5 → 丢
        let near = vec![bb(0., 0., 100., 100.), bb(20., 0., 120., 100.)];
        assert_eq!(dedupe_overlapping(&near, SEAL_IOU_DEDUPE), vec![0]);
        // 右移 80 → 交叠 20x100，IoU = 2000/18000 ≈ 0.111 < 0.5 → 留
        let far = vec![bb(0., 0., 100., 100.), bb(80., 0., 180., 100.)];
        assert_eq!(dedupe_overlapping(&far, SEAL_IOU_DEDUPE), vec![0, 1]);
    }

    #[test]
    fn empty_input_yields_empty_kept() {
        assert!(dedupe_overlapping(&[], SEAL_IOU_DEDUPE).is_empty());
    }

    #[test]
    fn block_joins_trimmed_deduped_lines() {
        let lines = vec![
            "  XX有限公司".to_string(),
            "".to_string(),
            "XX有限公司".to_string(), // 精确重复 → 去重
            "合同专用章".to_string(),
        ];
        assert_eq!(
            join_seal_lines(&lines).as_deref(),
            Some("XX有限公司 合同专用章")
        );
    }

    /// 全空（检测到了框但一行没识别出来）→ None，不写回也不留空前缀。
    #[test]
    fn block_none_when_no_content() {
        assert_eq!(join_seal_lines(&[]), None);
        assert_eq!(join_seal_lines(&["   ".to_string(), "".to_string()]), None);
    }

    // ── 弧行判定 ──

    /// 合成环排弧带（DB 轮廓形态：外缘弧 + 径向边 + 内缘回程，126°/带宽 34）。
    fn arc_band(cx: f32, cy: f32, r_hi: f32, band: f32, a0: f32, a1: f32, half: usize) -> Vec<Point> {
        let r_lo = r_hi - band;
        let mut pts = Vec::with_capacity(2 * half + 2);
        for i in 0..=half {
            let t = a0 + (a1 - a0) * i as f32 / half as f32;
            pts.push(Point::new(cx + r_hi * t.cos(), cy + r_hi * t.sin()));
        }
        for i in 0..=half {
            let t = a1 + (a0 - a1) * i as f32 / half as f32;
            pts.push(Point::new(cx + r_lo * t.cos(), cy + r_lo * t.sin()));
        }
        pts
    }

    /// 环排（章顶/章底两侧弧、深弧与浅弧）一律判为弧行 → 跳过，不产出残缺字。
    #[test]
    fn curved_bands_are_detected_top_and_bottom() {
        let top = arc_band(200., 200., 140., 34., -2.26, -0.06, 45); // 126° 章顶
        assert!(is_curved_band(&top), "章顶环排 cover={:.2}", quad_cover_ratio(&top));
        let bottom = arc_band(200., 160., 150., 30., 0.47, 2.67, 60); // 126° 章底
        assert!(is_curved_band(&bottom), "章底环排 cover={:.2}", quad_cover_ratio(&bottom));
        let shallow = arc_band(200., 150., 120., 40., -std::f32::consts::PI - 0.6, -std::f32::consts::PI + 0.6, 30); // 左侧 69°
        assert!(is_curved_band(&shallow), "浅弧 cover={:.2}", quad_cover_ratio(&shallow));
    }

    /// 近似直的行框（4 点 quad、栅格矩形、极小角跨）→ 判直，走 min-area-rect。
    #[test]
    fn straight_rows_are_not_curved() {
        let quad = vec![
            Point::new(0., 0.),
            Point::new(50., 0.),
            Point::new(50., 20.),
            Point::new(0., 20.),
        ];
        assert!(!is_curved_band(&quad));
        // 实心矩形栅格轮廓（沿边走行）
        let mut ring = Vec::new();
        for x in 0..30 {
            ring.push(Point::new(x as f32 * 4., 0.));
        }
        for y in 0..20 {
            ring.push(Point::new(116., y as f32 * 4.));
        }
        for x in (0..30).rev() {
            ring.push(Point::new(x as f32 * 4., 76.));
        }
        for y in (0..20).rev() {
            ring.push(Point::new(0., y as f32 * 4.));
        }
        assert!(quad_cover_ratio(&ring) > 0.95, "矩形栅格 cover 应 ≈1");
        assert!(!is_curved_band(&ring));
        // 小角跨（≈15°，实测 cover ≈0.90 的「专用章」同级）
        let nearly_straight = arc_band(200., 200., 140., 34., 1.44, 1.70, 30);
        assert!(
            !is_curved_band(&nearly_straight),
            "≈15° 角跨应判直（cover={:.2}）",
            quad_cover_ratio(&nearly_straight)
        );
    }

    /// 退化点集（重合点、零面积）→ cover=0 但不应 panic；判定为弧行 = 跳过，
    /// 与「识别不出」同样安全（调用方本就对裁剪/识别失败降级）。
    #[test]
    fn degenerate_input_does_not_panic() {
        assert_eq!(quad_cover_ratio(&[]), 0.0);
        let flat: Vec<Point> = (0..24).map(|_| Point::new(5., 5.)).collect();
        assert_eq!(quad_cover_ratio(&flat), 0.0);
        let _ = is_curved_band(&flat);
    }

    /// 口径说明（后续做弧行矫正时要用）：cover 度量的是**带结构**而非圆度——
    /// 整圆轮廓填满其 min-area-rect 到 π/4≈0.785，**高于**阈值，故不会被本判据
    /// 拦下。真实印章检测不输出整圈轮廓（实测只输出沿弧走行的文字带），
    /// 这里把这条例外固化下来，避免后来者误以为本判据能兜住任意圆形多边形。
    #[test]
    fn cover_gate_measures_bandness_not_roundness() {
        let disc: Vec<Point> = (0..40)
            .map(|i| {
                let t = TAU * i as f32 / 40.0;
                Point::new(100. + 60. * t.cos(), 100. + 60. * t.sin())
            })
            .collect();
        let c = quad_cover_ratio(&disc);
        assert!((c - std::f32::consts::FRAC_PI_4).abs() < 0.02, "整圆 cover={c:.3} 应 ≈π/4");
        assert!(!is_curved_band(&disc), "阈值 0.7 之下才有弧行判定");
        // 近满环带（300° 弧带，字带真正会退化成的形态）→ 判弧行
        let near_ring = arc_band(200., 200., 140., 34., -1.0, -1.0 + 5.24, 60);
        assert!(is_curved_band(&near_ring), "300° 环带 cover={:.2}", quad_cover_ratio(&near_ring));
    }
}
