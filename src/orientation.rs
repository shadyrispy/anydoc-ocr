//! 文字层朝向分组（借鉴 MinerU `tables.py::_detect_table_angle_from_pdf_lines`）。
//!
//! MinerU 的做法：表格 bbox 已知 → 统计与表格重叠 >=50% 的原生文字行的
//! `rotation`，在 {0,90,180,270} 上投票（容差 0.1°，`_is_supported_rotation`），
//! 有效行 >=3（`TABLE_TEXT_ORIENTATION_MIN_VALID_LINES`）才采信，否则回落视觉
//! cls 模型。我们文字层通路没有"表格 bbox 的原生行"（版面框未定），但同一组
//! 常量正好用来解决一个更基本的问题：**一页上有多种文字朝向**。
//!
//! 触发场景（现网真实缺陷，见 tests/orientation.rs）：
//! - 正立页内画了一张旋转 90° 的表 → 旧路径把整页当一次网格重建，正立正文与
//!   旋转表块混进同一批块 → 得到**转置的**表（行列互换），正文被吞进格子；
//! - 整页主体旋转（pdf-inspector 会把帧转正，见 `PositionFrame::Sheet`）→ 页上
//!   残留的正立文字（表头、页眉）在转正后的帧里是竖排，与表格混在同一次重建 →
//!   表退化成单列，或正文整体丢失。
//!
//! 与 MinerU 的取舍差异（有意为之）：**不用** `MIN_DOMINANCE_RATIO`。该闸在
//! MinerU 侧防的是"单一角度结论不可信"；此处语义是"要不要分组"，五五开的页面
//! 照样该分（两组各自内部一致），噪声已由 [`MIN_GROUP_ITEMS`] 挡住。
//!
//! 本模块只做**纯几何/纯角度**判定（输入是 `rotation` 角序列，不依赖
//! pdf-inspector 类型），故可完整单测；调用方负责按组取用 item 与坐标变换。

/// 角度吸附容差（度）：`|deg - a| < 0.1`，a ∈ {0,90,180,270}。
/// 对齐 MinerU `_is_supported_rotation`（`utils/tables.py`）与其
/// `text/native.py` 的同名判定。
pub const ROTATION_TOLERANCE_DEG: f32 = 0.1;

/// 一个朝向组要独立成组所需的最少条目数。
/// 对齐 MinerU `TABLE_TEXT_ORIENTATION_MIN_VALID_LINES = 3`——单个竖排字符
/// （或一次误标）不该自成一组、被当成"另一种朝向的正文"。
pub const MIN_GROUP_ITEMS: usize = 3;

/// 把任意 baseline 角吸附到最近的支持角（0/90/180/270）。
///
/// 先按 360 取正模（负角与超范围角折叠），再吸附；离四个直角都 >= 容差
/// （轻微歪斜的扫描/手工旋转）→ `None`，调用方按正立处理（MinerU 同口径：
/// 不支持的旋转不参与投票）。
pub fn snap_rotation(degrees: f32) -> Option<u16> {
    if !degrees.is_finite() {
        return None;
    }
    let folded = degrees.rem_euclid(360.0);
    for a in [0u16, 90, 180, 270] {
        if (folded - a as f32).abs() < ROTATION_TOLERANCE_DEG {
            return Some(a);
        }
    }
    // 360 与 0 同角（rem_euclid 后 folded < 360，仅极端浮点情形落到此处）
    if (360.0 - folded).abs() < ROTATION_TOLERANCE_DEG {
        return Some(0);
    }
    None
}

/// 朝向分组：输入同页各文字的 baseline 角（度），输出 `(吸附角, 条目下标)` 列表。
///
/// 规则（结果与输入顺序无关、可复现）：
/// 1. 逐条吸附（[`snap_rotation`]），吸不动的（非直角/NaN）按正立 0° 归组；
/// 2. 条目数 >= [`MIN_GROUP_ITEMS`] 的角度各自独立成组，按组大小降序、同大小
///    按角度升序输出；
/// 3. 不足 `MIN_GROUP_ITEMS` 的零散条目并入首个大组（全页都零散时自成一元组）；
/// 4. 全页同角 → 恰一个组——调用方据此判定"无需分组"，走历史整页路径，
///    输出逐字节不变。
///
/// 返回值长度 <= 4，各项 angle 互异、下标互不重叠、并集 = 全部输入下标。
pub fn vote_groups(angles: &[f32]) -> Vec<(u16, Vec<usize>)> {
    if angles.is_empty() {
        return Vec::new();
    }
    let mut buckets: Vec<(u16, Vec<usize>)> = Vec::new();
    for (idx, &deg) in angles.iter().enumerate() {
        let angle = snap_rotation(deg).unwrap_or(0);
        match buckets.iter_mut().find(|(a, _)| *a == angle) {
            Some((_, items)) => items.push(idx),
            None => buckets.push((angle, vec![idx])),
        }
    }
    // 组大小降序；同大小按角度升序（0° 优先），保证与输入顺序无关且可复现。
    buckets.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));
    let mut out: Vec<(u16, Vec<usize>)> = Vec::new();
    let mut noise: Vec<usize> = Vec::new();
    for (angle, mut items) in buckets {
        if items.len() >= MIN_GROUP_ITEMS {
            items.sort_unstable();
            out.push((angle, items));
        } else {
            noise.append(&mut items);
        }
    }
    if !noise.is_empty() {
        noise.sort_unstable();
        match out.first_mut() {
            Some((_, items)) => {
                items.extend(noise);
                items.sort_unstable();
            }
            None => out.push((0, noise)),
        }
    }
    out
}

/// 把 `y` 轴向上的页帧盒 `(x, y, w, h)`（`x/y` 为左下角）旋转进"该朝向正立"
/// 的帧，即把 baseline 从 `angle` 转回 0°（顺时针 `angle`）。
///
/// 变换绕原点做，结果可能是负坐标——由调用方平移到原点。与调用侧既有约定
/// 配套：变换后仍用 `Region::from_top_left(x, -y, w, h, text)` 落到"y 越小
/// 越靠上"的阅读坐标。角度只认 [`snap_rotation`] 的四个直角；其余原样返回
/// （调用方不会传入，吸不动的已在吸附时归 0°）。
pub fn to_upright_box(x: f32, y: f32, w: f32, h: f32, angle: u16) -> (f32, f32, f32, f32) {
    // 四角旋转 -angle 后取包围盒（仍 y 向上、给回左下角 + 宽高）。
    match angle {
        0 => (x, y, w, h),
        // 顺时针 90°：(X, Y) → (Y, -X)
        90 => (y, -x - w, h, w),
        // 180°：(X, Y) → (-X, -Y)
        180 => (-x - w, -y - h, w, h),
        // 逆时针 90°（= 顺时针 270°）：(X, Y) → (-Y, X)
        270 => (-y - h, x, h, w),
        _ => (x, y, w, h),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snaps_only_the_four_right_angles() {
        assert_eq!(snap_rotation(0.0), Some(0));
        assert_eq!(snap_rotation(90.0), Some(90));
        assert_eq!(snap_rotation(180.0), Some(180));
        assert_eq!(snap_rotation(270.0), Some(270));
        assert_eq!(snap_rotation(-90.0), Some(270), "负角折叠同渲染器");
        assert_eq!(snap_rotation(360.0), Some(0));
        assert_eq!(snap_rotation(450.0), Some(90));
        // 容差外（轻微歪斜）→ 不支持
        assert_eq!(snap_rotation(0.2), None);
        assert_eq!(snap_rotation(89.8), None);
        assert_eq!(snap_rotation(45.0), None);
        assert_eq!(snap_rotation(f32::NAN), None);
        assert_eq!(snap_rotation(f32::INFINITY), None);
    }

    #[test]
    fn uniform_page_is_a_single_group() {
        let angles = vec![0.0; 10];
        let g = vote_groups(&angles);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].0, 0);
        assert_eq!(g[0].1.len(), 10);
    }

    /// 正立正文 + 旋转 90° 表块 → 两组分离，表块不再与正文混进同一次网格重建。
    /// 输出按组大小降序（谁大谁在前），调用方另按"0° 优先"决定输出序。
    #[test]
    fn rotated_block_splits_from_upright_body() {
        let mut angles = vec![0.0; 18];
        angles.extend(std::iter::repeat_n(90.0, 21));
        let g = vote_groups(&angles);
        assert_eq!(g.len(), 2, "{g:?}");
        assert_eq!((g[0].0, g[0].1.len()), (90, 21), "大组在前");
        assert_eq!((g[1].0, g[1].1.len()), (0, 18));
        assert_eq!(g[0].1, (18..39).collect::<Vec<_>>());
        assert_eq!(g[1].1, (0..18).collect::<Vec<_>>());
        // 下标不重叠、并集完整
        let mut seen = vec![0usize; 39];
        for (_, ids) in &g {
            for &i in ids {
                seen[i] += 1;
            }
        }
        assert!(seen.iter().all(|&c| c == 1), "{seen:?}");
    }

    /// 不足 MIN_GROUP_ITEMS 的朝向（单条竖排残字）并入主组，不独立成组。
    #[test]
    fn tiny_group_merges_into_dominant() {
        let mut angles = vec![0.0; 12];
        angles.push(90.0);
        let g = vote_groups(&angles);
        assert_eq!(g.len(), 1, "{g:?}");
        assert_eq!(g[0].1.len(), 13);
        assert!(g[0].1.contains(&12));
    }

    /// 五五开页面照样分组（有意不设 MinerU 的 dominant-ratio 闸）：三组各 3 条
    /// → 三个组，互不吞并，每组内部一致。
    #[test]
    fn balanced_page_still_groups() {
        let mut angles = vec![0.0; 3];
        angles.extend(std::iter::repeat_n(90.0, 3));
        angles.extend(std::iter::repeat_n(180.0, 3));
        let g = vote_groups(&angles);
        assert_eq!(g.len(), 3, "{g:?}");
        // 同大小 → 角度升序
        assert_eq!(
            g.iter().map(|(a, _)| *a).collect::<Vec<_>>(),
            vec![0, 90, 180]
        );
        for (_, ids) in &g {
            assert_eq!(ids.len(), 3);
        }
    }

    /// 转正帧（MinerU 同族场景）：主组 0°=表 24 条、残留正立正文吸附后 270°
    /// 共 6 条 → 两组，主组占比 0.8 >= 0.6。
    #[test]
    fn turned_frame_separates_stray_upright_text() {
        let mut angles = vec![0.0; 24];
        angles.extend(std::iter::repeat_n(270.0, 6));
        let g = vote_groups(&angles);
        assert_eq!(g.len(), 2);
        assert_eq!((g[0].0, g[0].1.len()), (0, 24));
        assert_eq!((g[1].0, g[1].1.len()), (270, 6));
    }

    #[test]
    fn empty_input_returns_no_groups() {
        assert!(vote_groups(&[]).is_empty());
    }

    /// 四角旋转的盒变换保面积，且正立角恒等（守护"默认路径零扰动"）；
    /// 与反向角复合应回到原盒（可逆性）。
    #[test]
    fn box_rotation_preserves_area_and_identity() {
        let (x, y, w, h) = (10.0_f32, 20.0, 30.0, 8.0);
        assert_eq!(to_upright_box(x, y, w, h, 0), (x, y, w, h));
        for a in [0u16, 90, 180, 270] {
            let (nx, ny, nw, nh) = to_upright_box(x, y, w, h, a);
            assert!((nw * nh - w * h).abs() < 1e-4, "{a}: {nx} {ny} {nw} {nh}");
            // -angle 之后再转回 +angle（= 再旋转 360-angle）应回到原盒
            let (rx, ry, rw, rh) = to_upright_box(nx, ny, nw, nh, (360 - a) % 360);
            assert!(
                (rx - x).abs() < 1e-3 && (ry - y).abs() < 1e-3
                    && (rw - w).abs() < 1e-3 && (rh - h).abs() < 1e-3,
                "{a}: {rx} {ry} {rw} {rh}"
            );
        }
    }

    /// 90° 盒：宽与高互换、位置按 (X,Y)→(Y,-X) 落位；竖长条变横长条。
    #[test]
    fn ninety_degrees_turns_a_vertical_run_horizontal() {
        // 竖排长条：宽 11（字高）、高 261（行宽），baseline 角 90°
        let (nx, ny, nw, nh) = to_upright_box(60.0, 700.0, 11.0, 261.0, 90);
        assert_eq!((nx, ny, nw, nh), (700.0, -71.0, 261.0, 11.0));
        assert!(nh < nw, "转正后应为横排");
    }
}
