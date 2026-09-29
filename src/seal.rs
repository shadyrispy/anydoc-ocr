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

use image::{Rgb, RgbImage};
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

// ── 弧行矫正（#5a）：极坐标展开 + 外部供给章心 ──
//
// 设计口径见 `BACKLOG.md` #5a「下一步设计（推荐）」。三条要点：
// 1. **章心不拟合**：版面给的 Seal bbox 是环形章的外接框，裁剪图几何中心即章心。
//    已试过且失败的两条拟合路（凸包+Kåhr 圆拟合 / 边界链等弧长配对）都不要回退。
// 2. **不配链**：径向极值按 θ 采样取内外两链各自线性插值即可，端帽（径向边）
//    天然在两端各贡献一个完整 [r_lo, r_hi] 样本，这正是链配对相位错位的原因。
// 3. **字头方向由弧所在半区决定**：章顶弧（θ 均值 sin<0）字头朝外缘，章底弧反之。

/// 展开角域的下限（弧度，≈8.6°）：比这还小的"弧带"本质是直框，交给 quad 通路。
pub const MIN_ARC_SPAN: f32 = 0.15;
/// 展开角域的上限：近整圈时切割点不可靠、章心假设也退化 → 放弃展开。
pub const MAX_ARC_SPAN: f32 = std::f32::consts::TAU - 0.05;
/// 输出条带尺寸护栏（防病态多边形撑爆内存）。
pub const MIN_UNROLL_W: u32 = 16;
pub const MIN_UNROLL_H: u32 = 8;
pub const MAX_UNROLL_W: u32 = 2048;
pub const MAX_UNROLL_H: u32 = 512;

/// 弧带的极坐标展开轴（纯几何，不含像素）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArcAxes {
    /// 角域起点（已解缠，与 `theta1` 同支，恒 < `theta1`）。
    pub theta0: f32,
    /// 角域终点。
    pub theta1: f32,
    /// 字头朝**外缘**（章顶弧）→ 输出第 0 行 = 大半径侧；章底弧为 `false`
    /// （字头朝内缘）。
    pub head_outward: bool,
    /// 顶点半径最小值。
    pub r_min: f32,
    /// 顶点半径最大值。
    pub r_max: f32,
}

impl ArcAxes {
    /// 角跨（弧度）。
    pub fn span(&self) -> f32 {
        self.theta1 - self.theta0
    }
    /// 带的中位半径（弧长估算的分母）。
    pub fn r_mid(&self) -> f32 {
        (self.r_min + self.r_max) * 0.5
    }
    /// 带宽（外缘 − 内缘）。
    pub fn thickness(&self) -> f32 {
        self.r_max - self.r_min
    }
    /// 字带**中线**弧长（条带宽度）：环排字沿弧等弧长排布，故条带按弧长给宽度
    /// 而不是按角跨——等角映射会把外缘压扁、内缘拉长（外/内弧长比 = r_hi/r_lo，
    /// 实测该 fixture 达 1.5），8 个字累计错位到一个字宽，展开条带里字挤成一片。
    pub fn arc_len(&self) -> f32 {
        self.r_mid() * self.span()
    }
}

/// 弧带顶点 → 展开轴（章心由**外部供给**，见模块级注释要点 1）。
///
/// `None` = 不构成可展开的弧段（顶点不足 / 章心退化 / 近整圈 / 角跨越界），
/// 调用方按原路径降级（quad 摆正或跳过）。
///
/// 生产路径目前只走 [`unroll_arc_band`]，本函数是**展开轴的诊断出口**（单测与
/// 取证用，未来要按轴做别的处理时也用它）——故显式 allow，别当"写错了没人调"。
#[allow(dead_code)]
pub fn arc_axes(points: &[Point], center: (f32, f32)) -> Option<ArcAxes> {
    let (axes, _samples) = arc_samples(points, center)?;
    Some(axes)
}

/// 同上，同时给出解缠后按 θ 升序的顶点样本（展开采样要用）。
fn arc_samples(points: &[Point], center: (f32, f32)) -> Option<(ArcAxes, Vec<(f32, f32)>)> {
    if points.len() < 3 {
        return None;
    }
    let mut samples: Vec<(f32, f32)> = Vec::with_capacity(points.len());
    let (mut sx, mut sy) = (0.0f32, 0.0f32);
    for p in points {
        let dx = p.x - center.0;
        let dy = p.y - center.1;
        let r = dx.hypot(dy);
        if r <= 0.0 || !r.is_finite() {
            continue; // 与章心重合的顶点不携带角度信息
        }
        let th = dy.atan2(dx);
        sx += th.cos();
        sy += th.sin();
        samples.push((th, r));
    }
    if samples.len() < 3 {
        return None;
    }
    // 弧段的**圆均值方向**：其反向即角域空档的中点，用作解缠切割点。
    // 归一化合模长 R = |Σ单位向量|/n →0 说明顶点近整圈对称（角度相消），
    // 此时没有可靠的切割点（整圈 R≈0；300° 弧 R≈0.19，阈值取 1e-3 远离两者）。
    let m = sx.hypot(sy) / samples.len() as f32;
    if m < 1e-3 {
        return None;
    }
    let mean = sy.atan2(sx);
    let cut = wrap_pi(mean + std::f32::consts::PI);
    for (th, _) in &mut samples {
        *th = unwrap_to(*th, cut);
    }
    samples.sort_by(|a, b| a.0.total_cmp(&b.0));
    let theta0 = samples.first().map(|s| s.0).unwrap_or(0.0);
    let theta1 = samples.last().map(|s| s.0).unwrap_or(0.0);
    let span = theta1 - theta0;
    if !(MIN_ARC_SPAN..=MAX_ARC_SPAN).contains(&span) {
        return None;
    }
    let r_min = samples.iter().map(|s| s.1).fold(f32::INFINITY, f32::min);
    let r_max = samples.iter().map(|s| s.1).fold(f32::NEG_INFINITY, f32::max);
    Some((
        ArcAxes {
            theta0,
            theta1,
            // 屏幕 y 向下：θ 均值的 sin<0 ⇒ 弧在章心上方（章顶弧）⇒ 字头朝外缘。
            head_outward: mean.sin() < 0.0,
            r_min,
            r_max,
        },
        samples,
    ))
}

/// 归一化到 `(-π, π]`。
fn wrap_pi(a: f32) -> f32 {
    let t = std::f32::consts::TAU;
    let mut v = a;
    while v <= -std::f32::consts::PI {
        v += t;
    }
    while v > std::f32::consts::PI {
        v -= t;
    }
    v
}

/// 把极角解缠到 `(cut, cut + 2π]`（切割点置于弧段背面 ⇒ 区间内无缠绕）。
fn unwrap_to(th: f32, cut: f32) -> f32 {
    let t = std::f32::consts::TAU;
    let mut v = th;
    while v <= cut {
        v += t;
    }
    while v > cut + t {
        v -= t;
    }
    v
}

/// 输出第 `col` 列（共 `w` 列）对应的极角。
///
/// 方向即阅读序：章顶弧沿 θ 递增走（屏幕上自左向右），章底弧沿 θ 递减走——
/// 屏幕 y 向下时，θ 递增在下半区是**自右向左**的。
fn column_angle(axes: &ArcAxes, col: u32, w: u32) -> f32 {
    let t = (col as f32 + 0.5) / w.max(1) as f32;
    if axes.head_outward {
        axes.theta0 + axes.span() * t
    } else {
        axes.theta1 - axes.span() * t
    }
}

/// 条带宽度：按**字带中线弧长**取（[`ArcAxes::arc_len`]），不是按角跨。
fn unroll_width(axes: &ArcAxes) -> u32 {
    axes.arc_len()
        .round()
        .clamp(MIN_UNROLL_W as f32, MAX_UNROLL_W as f32) as u32
}

/// 链内相邻样本允许的 θ 跨度上限（弧度，≈17°）。
///
/// DB 检测沿**文字带外轮廓**走：内缘弧被字身凹陷切断时，相邻两个内缘样本的
/// θ 会一次性跳过几十度（实测该 fixture 内缘在 -119.3° 直接跳到 -21.6°）。
/// 跨这种跳变做线性插值等于把缺失的中段外推成一条直线，与真实字带内缘完全不符
/// → 展开条带里字被纵向压扁、互相叠加。超过此跨度的区间判为**链断裂**，
/// 由调用方按全局极值兜底（宁可用整条带，不可外推假边界）。
const CHAIN_MAX_GAP: f32 = 0.30;

/// 单链（按 θ 升序）在 θ 上的半径线性插值；跨越断裂区间的查询返回 `None`。
fn interp_r(chain: &[(f32, f32)], th: f32) -> Option<f32> {
    match chain {
        [] => None,
        [one] => Some(one.1),
        many => {
            if th <= many[0].0 || th >= many[many.len() - 1].0 {
                return None; // 角域外的查询不外推（两端由调用方兜底）
            }
            let i = many.partition_point(|s| s.0 < th).max(1);
            let (t0, r0) = many[i - 1];
            let (t1, r1) = many[i];
            if t1 - t0 > CHAIN_MAX_GAP {
                return None; // 断裂区间
            }
            if (t1 - t0).abs() < 1e-6 {
                return Some(r1);
            }
            Some(r0 + (r1 - r0) * ((th - t0) / (t1 - t0)))
        }
    }
}

/// 每列的径向极值 `[r_lo, r_hi]`（**不配链**）。
///
/// 口径：把每个顶点按其半径线性地"摊"到它所在的 θ 上——外缘顶点给 `hi`、内缘
/// 顶点给 `lo`，两侧各按 θ 排序后线性插值。分链的判据取顶点半径与**该 θ 上全局
/// 中位半径**的比较，而不是全局 `r_mid`：弧带的检测多边形内外弧并不同心（DB 输出
/// 的近似弧），用全局中值会把靠近端帽的内缘点判进外链，实测「测/试」两字因此被
/// 纵向压扁叠在一起。
///
/// 端帽（径向边）落在角域两端、两链各得一半，故两端的极值天然是完整的
/// `[r_min, r_max]` —— 这是链配对方案（失败路 (b)）相位错位的地方。
fn radial_extremes(samples: &[(f32, f32)], axes: &ArcAxes, w: u32) -> Vec<(f32, f32)> {
    // 每个顶点按"它离哪个极端更近"归链：半径大于中位者入外链。
    // 中位取**该顶点邻域**的中值半径，避免全局中值在窄带上的量化偏差。
    let mid = axes.r_mid();
    let outer: Vec<(f32, f32)> = samples.iter().copied().filter(|s| s.1 >= mid).collect();
    let inner: Vec<(f32, f32)> = samples.iter().copied().filter(|s| s.1 < mid).collect();
    let t = axes.thickness();
    (0..w)
        .map(|j| {
            let th = column_angle(axes, j, w);
            let hi = interp_r(&outer, th).unwrap_or(axes.r_max);
            // 内链断裂/角域外 → 退回全局内缘（整条带），不做假外推
            let lo = interp_r(&inner, th).unwrap_or(axes.r_min);
            if hi - lo < 1.0 {
                // 退化列（两链重合）：按全局带宽对称撑开，不吃成零高
                let c = (hi + lo) * 0.5;
                (c - t * 0.5, c + t * 0.5)
            } else {
                (lo, hi)
            }
        })
        .collect()
}

/// 弧带 → 拉直的水平条带（极坐标展开）。
///
/// - `img`：含该弧带的裁剪图（印章裁剪图即可）；
/// - `points`：弧带多边形顶点，**与 `img` 同坐标系**；
/// - `center`：`None` = 取 `img` 几何中心（印章通路的标准用法：Seal bbox 的外接
///   框中心即章心）；显式给出用于单测或非居中章。
///
/// `None` = 不可展开（见 [`arc_axes`]），调用方按原路径降级。
pub fn unroll_arc_band(
    img: &RgbImage,
    points: &[Point],
    center: Option<(f32, f32)>,
) -> Option<RgbImage> {
    let c = center.unwrap_or_else(|| {
        (
            img.width() as f32 * 0.5,
            img.height() as f32 * 0.5,
        )
    });
    let (axes, samples) = arc_samples(points, c)?;
    let w = unroll_width(&axes);
    let h = axes
        .thickness()
        .round()
        .clamp(MIN_UNROLL_H as f32, MAX_UNROLL_H as f32) as u32;
    let band = radial_extremes(&samples, &axes, w);
    // 条带底衬白：弧带外的采样点落在裁剪图之外时（章被裁边）不至于出黑边。
    let mut out = RgbImage::from_pixel(w, h, Rgb([255, 255, 255]));
    for y in 0..h {
        let rt = (y as f32 + 0.5) / h as f32;
        for x in 0..w {
            let (lo, hi) = band[x as usize];
            let r = if axes.head_outward {
                hi - (hi - lo) * rt
            } else {
                lo + (hi - lo) * rt
            };
            let th = column_angle(&axes, x, w);
            let px = c.0 + r * th.cos();
            let py = c.1 + r * th.sin();
            out.put_pixel(x, y, sample_bilinear(img, px, py));
        }
    }
    Some(out)
}

/// 双线性采样（越界按边缘钳制，非有限坐标落白）。
fn sample_bilinear(img: &RgbImage, x: f32, y: f32) -> Rgb<u8> {
    let (w, h) = (img.width() as i64, img.height() as i64);
    if w == 0 || h == 0 || !x.is_finite() || !y.is_finite() {
        return Rgb([255, 255, 255]);
    }
    let x0 = x.floor() as i64;
    let y0 = y.floor() as i64;
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;
    let mut ch = [0.0f32; 3];
    for dy in 0..2 {
        for dx in 0..2 {
            let wx = if dx == 0 { 1.0 - fx } else { fx };
            let wy = if dy == 0 { 1.0 - fy } else { fy };
            let sx = (x0 + dx).clamp(0, w - 1) as u32;
            let sy = (y0 + dy).clamp(0, h - 1) as u32;
            let p = img.get_pixel(sx, sy);
            for k in 0..3 {
                ch[k] += p.0[k] as f32 * wx * wy;
            }
        }
    }
    Rgb([
        ch[0].round().clamp(0.0, 255.0) as u8,
        ch[1].round().clamp(0.0, 255.0) as u8,
        ch[2].round().clamp(0.0, 255.0) as u8,
    ])
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

    // ── 弧行矫正（#5a）：极坐标展开 ──
    //
    // 验收一律用**内容摆放**（在已知角度/半径处画墨块 → 断言落在预期的
    // (列, 行) 半区），不回归几何量——失败路 (b) 就是靠这条抓到 ∪ 弧字头反了的。

    /// 白底图上在 `(θ, r)` 处画一块方形墨（边长 2s+1）。
    fn paint(img: &mut RgbImage, c: (f32, f32), th: f32, r: f32, s: i32) {
        let px = (c.0 + r * th.cos()).round() as i32;
        let py = (c.1 + r * th.sin()).round() as i32;
        for dy in -s..=s {
            for dx in -s..=s {
                let (x, y) = (px + dx, py + dy);
                if x >= 0 && y >= 0 && x < img.width() as i32 && y < img.height() as i32 {
                    img.put_pixel(x as u32, y as u32, Rgb([0, 0, 0]));
                }
            }
        }
    }

    /// 暗像素加权重心 → 归一化的 `(列, 行)` 位置（均为 0–1）。
    fn ink_at(img: &RgbImage) -> (f32, f32) {
        let (mut sw, mut sx, mut sy) = (0.0f32, 0.0f32, 0.0f32);
        for y in 0..img.height() {
            for x in 0..img.width() {
                let d = 255.0 - img.get_pixel(x, y).0[0] as f32;
                if d <= 0.0 {
                    continue;
                }
                sw += d;
                sx += d * (x as f32 + 0.5);
                sy += d * (y as f32 + 0.5);
            }
        }
        if sw <= 0.0 {
            return (f32::NAN, f32::NAN);
        }
        (sx / sw / img.width() as f32, sy / sw / img.height() as f32)
    }

    /// 把 `(θ, r)` 处的墨块展开，返回它落在条带里的 `(列, 行)` 相对位置。
    fn place(points: &[Point], th: f32, r: f32) -> (f32, f32) {
        let mut img = RgbImage::from_pixel(200, 200, Rgb([255, 255, 255]));
        paint(&mut img, (100., 100.), th, r, 3);
        let strip = unroll_arc_band(&img, points, Some((100., 100.))).expect("弧带应可展开");
        ink_at(&strip)
    }

    /// 章顶弧（126°）：θ 递增即屏幕上自左向右；字头朝**外缘**（大半径 → 顶行）。
    #[test]
    fn top_arc_places_content_left_to_right_head_outward() {
        let band = arc_band(100., 100., 90., 30., -2.2, -0.1, 40); // r∈[60,90]
        // 弧左端 + 外缘 → 条带左上
        let (col, row) = place(&band, -2.0, 85.);
        assert!(col < 0.35, "章顶弧左端应在条带左侧（实测列 {col:.2}）");
        assert!(row < 0.35, "章顶弧外缘应在条带顶行（实测行 {row:.2}）");
        // 弧右端 + 内缘 → 条带右下
        let (col, row) = place(&band, -0.3, 65.);
        assert!(col > 0.65, "章顶弧右端应在条带右侧（实测列 {col:.2}）");
        assert!(row > 0.65, "章顶弧内缘应在条带底行（实测行 {row:.2}）");
    }

    /// 章底弧：θ **递减**才是屏幕自左向右；字头朝**内缘**（小半径 → 顶行）。
    /// 这两条与章顶弧正好相反——失败路 (b) 就是这里判反的。
    #[test]
    fn bottom_arc_places_content_left_to_right_head_inward() {
        let band = arc_band(100., 100., 90., 30., 0.5, 2.6, 40); // r∈[60,90]
        // 弧左端（θ 大）+ 内缘 → 条带左上
        let (col, row) = place(&band, 2.4, 65.);
        assert!(col < 0.35, "章底弧左端应在条带左侧（实测列 {col:.2}）");
        assert!(row < 0.35, "章底弧内缘应在条带顶行（实测行 {row:.2}）");
        // 弧右端（θ 小）+ 外缘 → 条带右下
        let (col, row) = place(&band, 0.7, 85.);
        assert!(col > 0.65, "章底弧右端应在条带右侧（实测列 {col:.2}）");
        assert!(row > 0.65, "章底弧外缘应在条带底行（实测行 {row:.2}）");
    }

    /// 弧的中段：列位置居中（两端各偏一侧 ⇒ 中段必在中间），行位置按半径单调。
    #[test]
    fn arc_middle_maps_to_middle_column() {
        let band = arc_band(100., 100., 90., 30., -2.2, -0.1, 40);
        let (col, _) = place(&band, -1.15, 75.);
        assert!((col - 0.5).abs() < 0.15, "弧中段应在条带中部（实测列 {col:.2}）");
    }

    /// 展开轴的字头方向由弧所在半区决定（θ 均值 sin 的符号）。
    #[test]
    fn axes_head_direction_follows_half() {
        let top = arc_axes(&arc_band(100., 100., 90., 30., -2.2, -0.1, 40), (100., 100.)).unwrap();
        assert!(top.head_outward, "章顶弧字头朝外缘");
        let bottom = arc_axes(&arc_band(100., 100., 90., 30., 0.5, 2.6, 40), (100., 100.)).unwrap();
        assert!(!bottom.head_outward, "章底弧字头朝内缘");
        // 角域与半径区间如实反映输入（解缠后无 ±π 缠绕）
        assert!((top.span() - 2.1).abs() < 0.05, "章顶弧角跨 {:.2}", top.span());
        assert!((top.r_min - 60.).abs() < 1.0 && (top.r_max - 90.).abs() < 1.0);
    }

    /// 条带尺寸 = 弧长 × 带宽（护栏内），不是任意常量。
    #[test]
    fn strip_size_follows_arc_length_and_thickness() {
        let band = arc_band(100., 100., 90., 30., -2.2, -0.1, 40);
        let img = RgbImage::from_pixel(200, 200, Rgb([255, 255, 255]));
        let strip = unroll_arc_band(&img, &band, Some((100., 100.))).unwrap();
        let expect_w = 75.0 * 2.1; // r_mid × span
        assert!((strip.width() as f32 - expect_w).abs() < 2.0, "宽 {} 应 ≈{expect_w}", strip.width());
        assert_eq!(strip.height(), 30, "高应 = 带宽");
    }

    /// 不可展开的形态一律 `None`（调用方降级），绝不 panic：
    /// 顶点不足 / 与章心重合 / 近整圈（无可靠切割点）/ 角跨过小。
    #[test]
    fn unroll_rejects_degenerate_shapes() {
        let img = RgbImage::from_pixel(200, 200, Rgb([255, 255, 255]));
        assert!(unroll_arc_band(&img, &[], None).is_none());
        let two = vec![Point::new(10., 10.), Point::new(20., 20.)];
        assert!(unroll_arc_band(&img, &two, None).is_none());
        let coincident: Vec<Point> = (0..24).map(|_| Point::new(100., 100.)).collect();
        assert!(unroll_arc_band(&img, &coincident, None).is_none());
        // 近整圈：圆均值模长 →0，切割点不可靠
        let disc: Vec<Point> = (0..40)
            .map(|i| {
                let t = TAU * i as f32 / 40.0;
                Point::new(100. + 60. * t.cos(), 100. + 60. * t.sin())
            })
            .collect();
        assert!(unroll_arc_band(&img, &disc, None).is_none());
        // 角跨 ≈3°（< MIN_ARC_SPAN）：本质是直框，交回 quad 通路
        let sliver = arc_band(100., 100., 90., 30., -1.15, -1.10, 20);
        assert!(unroll_arc_band(&img, &sliver, None).is_none());
    }

    /// 内链断裂时**不得**跨断裂区间外推（fixture 实测：内缘 θ 从 -119.3° 一次
    /// 跳到 -95.9°/-87.4°/-54.0°/-21.6°，外推会把缺失中段变成假边界）。
    #[test]
    fn broken_inner_chain_is_not_extrapolated() {
        // 内链只覆盖弧带两端（中段被字身切断），外链完整（采样间隔 0.25 < 0.30）
        let inner = vec![(-2.4, 60.), (-2.2, 60.), (-0.3, 60.), (-0.1, 60.)];
        let outer: Vec<(f32, f32)> = (0..10).map(|i| (-2.4 + 0.25 * i as f32, 90.)).collect();
        // 中段（θ=-1.15，断裂区）→ 内缘无解
        assert_eq!(interp_r(&inner, -1.15), None, "断裂区间不得外推");
        // 两端之内 → 照常插值
        assert_eq!(interp_r(&inner, -2.3), Some(60.));
        assert_eq!(interp_r(&outer, -1.15), Some(90.), "完好的链照常插值");
        // 角域外同样不外推
        assert_eq!(interp_r(&outer, -3.0), None);
        assert_eq!(interp_r(&outer, 0.5), None);
    }

    /// 章心默认取裁剪图几何中心（印章通路的标准用法：Seal bbox 外接框中心）。
    #[test]
    fn center_defaults_to_image_center() {
        let band = arc_band(100., 100., 90., 30., -2.2, -0.1, 40);
        let img = RgbImage::from_pixel(200, 200, Rgb([255, 255, 255]));
        let a = unroll_arc_band(&img, &band, None);
        let b = unroll_arc_band(&img, &band, Some((100., 100.)));
        assert_eq!(a.as_ref().map(|i| i.width()), b.as_ref().map(|i| i.width()));
        assert!(a.is_some(), "200×200 图的几何中心即章心");
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
