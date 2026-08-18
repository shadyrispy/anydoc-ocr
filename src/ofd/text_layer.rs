//! OFD 文字层提取（P1.7 自 mod.rs 切分）：PageObject → `OfdTextLine` 行收集、
//! 斜排水印过滤（CTM 角度）、图片对象计数、乱码检测、Region 转换。
//!
//! 坐标约定：OFD 页面坐标系原点左上、y 轴向下，与 `reading_order` "小=上"
//! 约定一致（无需翻转；PDF 侧调用方才要翻转）。

use std::cmp::Ordering;

use ofd_core::model::graphics::PageBlock;
use ofd_core::model::page::PageObject;

use crate::region::Region;

/// OFD 文字层提取的文本行：x0, x1, y0, y1（左、右、上、下，文档坐标），text。
#[derive(Debug, Clone)]
pub(crate) struct OfdTextLine {
    pub x0: f64,
    pub x1: f64,
    pub y0: f64,
    pub y1: f64,
    pub text: String,
}

/// `OfdTextLine` 文本行 → `Region`（`f32` 区域，reading_order / table_grid 共用）。
pub(crate) fn to_regions(texts: Vec<OfdTextLine>) -> Vec<Region> {
    texts
        .into_iter()
        .map(|line| {
            Region::new(
                line.x0 as f32,
                line.x1 as f32,
                line.y0 as f32,
                line.y1 as f32,
                line.text,
            )
        })
        .collect()
}

/// F3 坏字体乱码检测：统计 U+FFFD 替换符 / 私有区（U+E000..U+F8FF）/ 控制字符。
/// 整页字符数须超过 [`crate::text_health::GARBLED_MIN_TOTAL_CHARS`] 且坏字符占比 >=
/// [`crate::text_health::GARBLED_BAD_PERCENT_THRESHOLD`]%（`bad*100 >= total*20`）才判乱码，
/// 避免少量误报（如目录点线符的私有区字符）触发整页 OCR。字符分类与阈值常量均收敛于
/// `text_health`（PDF/OFD 共用）。
pub(crate) fn is_garbled_text(texts: &[OfdTextLine]) -> bool {
    let chars = texts.iter().flat_map(|line| line.text.chars());
    crate::text_health::has_garbled_chars(
        chars,
        crate::text_health::GARBLED_MIN_TOTAL_CHARS,
        crate::text_health::GARBLED_BAD_PERCENT_THRESHOLD,
    )
}

/// 收集一页所有 TextObject 的文本，返回 `OfdTextLine`（`x0/x1/y0/y1/行文本`），
/// 坐标为**页面坐标**（原点左上、y 向下，与 `reading_order` "小=上" 约定一致）。
///
/// 区域优先取对象真实页面包围盒（`boundary`）：x_min=boundary.x，
/// x_max=boundary.x+width，y_min=boundary.y，y_max=boundary.y+height——行宽真实
/// 才能让跨整页的页眉/页脚命中 `reading_order::is_full` 的整宽判定。若某对象
/// boundary 宽/高退化（0/负数/NaN，应大于 0），退回单点区域
/// `(x, x, y, y+1.0)`（x/y 为首字符经 boundary 平移 + CTM 变换后的页面坐标），
/// 保证不 panic 也不产生退化区域。
///
/// `TextCode` 的 X/Y 是对象局部坐标（同一对象内相对原点），实际页面位置需经
/// 对象边界平移 + CTM 变换得出：`page = boundary + CTM(code)`。OFD 页面坐标系
/// 原点在左上、y 轴向下（`render` 的 `page_to_device` 直接把物理区左上角映射到
/// 设备原点），故返回的 y 已是"越小越靠上"，与 `reading_order` 约定一致。
pub(crate) fn collect_text_lines(page: &PageObject) -> Vec<OfdTextLine> {
    let mut out = Vec::new();
    if let Some(content) = &page.content {
        for layer in &content.layers {
            collect_text_blocks(&layer.objects, &mut out);
        }
    }
    out
}

/// 斜向旋转水印/装饰文字过滤：从 CTM 线性部分计算文本旋转角
/// （`atan2(b, a)`，单位度，归一化到 [0,360)），若偏离 {0,90,180,270}
/// 超过 ±12° 则视为斜排水印/装饰文字 → 返回 `true`（跳过）。
///
/// 保留 0°/180°（横排正文）与 90°/270°（竖排正文，如中文公文竖排）——
/// 竖排是合法正文，不得误删。CTM 缺省或畸形（非 6 元）一律视为轴对齐，
/// 返回 `false`（保留），与调用侧默认一致。角度 NaN 时比较均为 false → 保留。
///
/// 实现：`deg % 90.0` 到最近轴角度的角距，360↔0 环绕由取模自动处理；
/// 1e-9 容差吸收 cos/sin↔atan2 往返的浮点误差（如 348° 重构为
/// 347.99999999999994 的边界抖动），实际角度分辨率不受影响。
fn ctm_is_watermark_angle(ctm: &[f64]) -> bool {
    if ctm.len() != 6 {
        return false;
    }
    let deg = ctm[1].atan2(ctm[0]).to_degrees();
    let deg = (deg % 360.0 + 360.0) % 360.0; // 归一化到 [0,360)
    const TOL: f64 = 12.0;
    let r = deg % 90.0;
    let nearest_axis_dist = r.min(90.0 - r);
    nearest_axis_dist > TOL + 1e-9
}

fn collect_text_blocks(blocks: &[PageBlock], out: &mut Vec<OfdTextLine>) {
    for b in blocks {
        match b {
            PageBlock::Text(t) => {
                // 斜向旋转的 TextObject（如"太原市人民政府公报"对角水印）在抽取前直接跳过；
                // 竖排（90/270°）与横排（0/180°）正文不受影响。
                if let Some(m) = t.ctm.as_ref()
                    && ctm_is_watermark_angle(m.as_slice())
                {
                    continue;
                }
                let mut codes: Vec<(f64, &str)> = t
                    .text_codes
                    .iter()
                    .filter_map(|c| c.text.as_deref().map(|txt| (c.x.unwrap_or(0.0), txt)))
                    .collect();
                codes.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));
                let line: String = codes.iter().map(|(_, t)| *t).collect();
                if line.trim().is_empty() {
                    continue;
                }
                // TextCode 首字符局部坐标 → 页面坐标：boundary 平移 + CTM 变换。
                let (lx, ly) = t
                    .text_codes
                    .first()
                    .map(|c| (c.x.unwrap_or(0.0), c.y.unwrap_or(0.0)))
                    .unwrap_or((0.0, 0.0));
                let (a, b_, c, d, e, f) = match t.ctm.as_ref().map(|m| m.as_slice()) {
                    Some(m) if m.len() == 6 => (m[0], m[1], m[2], m[3], m[4], m[5]),
                    _ => (1.0, 0.0, 0.0, 1.0, 0.0, 0.0),
                };
                let x = t.boundary.x + a * lx + c * ly + e;
                let y = t.boundary.y + b_ * lx + d * ly + f;
                if t.boundary.width > 0.0 && t.boundary.height > 0.0 {
                    // 真实页面包围盒：让跨整页的页眉/页脚获得整行宽度。
                    let x0 = t.boundary.x;
                    let x1 = t.boundary.x + t.boundary.width;
                    let y0 = t.boundary.y;
                    let y1 = t.boundary.y + t.boundary.height;
                    out.push(OfdTextLine {
                        x0,
                        x1,
                        y0,
                        y1,
                        text: line,
                    });
                } else {
                    // boundary 退化：退回旧单点行为（首字符坐标），不 panic。
                    out.push(OfdTextLine {
                        x0: x,
                        x1: x,
                        y0: y,
                        y1: y + 1.0,
                        text: line,
                    });
                }
            }
            PageBlock::Block(g) => collect_text_blocks(&g.objects, out),
            _ => {}
        }
    }
}

/// 统计一页内 ImageObject 数量（用于页型判定）。
pub(crate) fn count_images(page: &PageObject) -> usize {
    let mut n = 0;
    if let Some(content) = &page.content {
        for layer in &content.layers {
            count_image_blocks(&layer.objects, &mut n);
        }
    }
    n
}

fn count_image_blocks(blocks: &[PageBlock], n: &mut usize) {
    for b in blocks {
        match b {
            PageBlock::Image(_) => *n += 1,
            PageBlock::Block(g) => count_image_blocks(&g.objects, n),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 角度 → CTM 线性矩阵 [cos,sin,-sin,cos,0,0]。
    fn ctm(deg: f64) -> [f64; 6] {
        let r = deg.to_radians();
        [r.cos(), r.sin(), -r.sin(), r.cos(), 0.0, 0.0]
    }

    #[test]
    fn ctm_watermark_angle_detection() {
        // 轴对齐横排：0°（单位矩阵）→ 保留
        assert!(!ctm_is_watermark_angle(&[1.0, 0.0, 0.0, 1.0, 0.0, 0.0]));
        // 竖排正文：90° / 270° → 保留
        assert!(!ctm_is_watermark_angle(&ctm(90.0)));
        assert!(!ctm_is_watermark_angle(&ctm(270.0)));
        // 反向横排：180° → 保留
        assert!(!ctm_is_watermark_angle(&ctm(180.0)));
        // 斜排水印：30° / 45° → 跳过
        assert!(ctm_is_watermark_angle(&ctm(30.0)));
        assert!(ctm_is_watermark_angle(&ctm(45.0)));
        // 容差边界：12° 内保留、超过 12° 跳过
        assert!(!ctm_is_watermark_angle(&ctm(12.0)));
        assert!(ctm_is_watermark_angle(&ctm(13.0)));
        // 360↔0 环绕边界：348°(= -12°) 保留、347° 跳过
        assert!(!ctm_is_watermark_angle(&ctm(348.0)));
        assert!(ctm_is_watermark_angle(&ctm(347.0)));
        // 畸形/缺省 CTM → 视为轴对齐保留
        assert!(!ctm_is_watermark_angle(&[1.0, 0.0]));
        assert!(!ctm_is_watermark_angle(&[]));
    }

    #[test]
    fn garbled_text_detection() {
        // 正常中文文本 → 不乱码
        let ok = vec![
            OfdTextLine {
                x0: 0.0,
                x1: 10.0,
                y0: 0.0,
                y1: 1.0,
                text: "太原市人民政府公报".to_string(),
            },
            OfdTextLine {
                x0: 0.0,
                x1: 10.0,
                y0: 1.0,
                y1: 2.0,
                text: "二〇二五年第一期".to_string(),
            },
        ];
        assert!(!is_garbled_text(&ok));
        // 60 个 U+FFFD 替换符（>50 字符且占比 100%）→ 乱码
        let bad: Vec<OfdTextLine> = (0..60)
            .map(|i| OfdTextLine {
                x0: 0.0,
                x1: 10.0,
                y0: i as f64,
                y1: i as f64 + 1.0,
                text: "\u{FFFD}".to_string(),
            })
            .collect();
        assert!(is_garbled_text(&bad));
        // 仅 10 个替换符（总量不足 50）→ 不判乱码
        assert!(!is_garbled_text(&bad[..10]));
        // 私有区字符（目录点线符常见）占比 <20%（10 坏 / 70 总）→ 不判乱码
        let mut mixed: Vec<OfdTextLine> = (0..60)
            .map(|i| OfdTextLine {
                x0: 0.0,
                x1: 10.0,
                y0: i as f64,
                y1: i as f64 + 1.0,
                text: "正常正文".to_string(),
            })
            .collect();
        for m in mixed.iter_mut().take(10) {
            m.text = "\u{E000}".to_string();
        }
        assert!(!is_garbled_text(&mixed));
    }

    #[test]
    fn title_prefix_applied() {
        let lines = vec![
            "一、总则".to_string(),
            "这是正文句子。".to_string(),
            "# 已带前缀的标题".to_string(),
        ];
        let out = crate::text_health::apply_title_prefixes(&lines, &[], true);
        assert_eq!(out[0], "## 一、总则");
        assert_eq!(out[1], "这是正文句子。");
        assert_eq!(out[2], "# 已带前缀的标题");
    }
}
