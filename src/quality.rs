//! ADR-0007：基于图像质量的 OCR 档位路由。
//!
//! 问题：单一默认档位（tiny/100）无法兼顾"清晰扫描件要快"与"污染扫描件要准"。
//! 方案：渲染前 N 页 → 算质量指标（归一化 Laplacian 方差 + 噪声 + 对比度）→
//! 阈值树分级 HIGH/MEDIUM/LOW → 映射到 (OcrTier, dpi)。仅作用于 OCR 路径，
//! 文字层通路不触发。
//!
//! 阈值取业界标准（4 源交叉验证）：Laplacian 归一化 0.5 ≈ 原始方差 100（分界线）；
//! 噪声方差 50 / 对比度 0.3 为污染判据。保守偏向，宁可升级不可降级。
//!
//! 卷积手写（Laplacian 3×3 / Sobel / Gaussian σ=1）：原先直接依赖 imageproc 0.25，
//! 与 oar-ocr 依赖的 0.27 形成 diamond dependency（Cargo.lock 多出 nalgebra/simba/wide/
//! safe_arch/rand/rand_distr/itertools 各两份）。质量路由仅用 3 个卷积函数，手写 ~40 行
//! 即可彻底移除直接依赖，消除双版本编译/体积/攻击面。pad 策略对齐 imageproc filter3x3
//! （clamp 索引 = continuity padding）。
use clap::ValueEnum;
use image::{GrayImage, ImageBuffer, Luma};

use crate::models::OcrTier;

/// 质量路由开关。`Off` 时退回 `--ocr-tier`/`--dpi` 显式值（golden 测试用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum QualityRoute {
    /// 自动评估前 N 页质量，路由到 (tier, dpi)
    #[default]
    Auto,
    /// 关闭路由，用显式参数
    Off,
}

/// 四指标：归一化 Laplacian 方差（模糊度核心）、噪声方差、对比度、平均锐度。
#[derive(Debug, Clone, Copy)]
pub struct QualityMetrics {
    /// `laplacian_filter` 方差 ÷ 原图灰度方差。跨 DPI 可比，>0.5 清晰。
    pub laplacian_norm: f32,
    /// 原图 - `gaussian_blur_f32(σ=1)` 残差的均值绝对值。>50 有污染。
    pub noise_var: f32,
    /// `(max - min) / 255`。动态范围，<0.3 偏暗/偏淡。
    pub contrast: f32,
    /// `sobel_gradients` 结果的均值。边缘清晰度。
    pub sharpness: f32,
}

/// 三级质量档。HIGH→tiny/100（快）、MEDIUM→tiny/150、LOW→small/100（准）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualityTier {
    High,
    Medium,
    Low,
}

impl QualityTier {
    /// 映射到 OCR 档位（ADR-0007 D3）。
    pub fn tier(&self) -> OcrTier {
        match self {
            QualityTier::High | QualityTier::Medium => OcrTier::Tiny,
            QualityTier::Low => OcrTier::Small,
        }
    }

    /// 映射到渲染 DPI（ADR-0007 D3）。
    pub fn dpi(&self, base: f32) -> f32 {
        match self {
            QualityTier::High => base,
            QualityTier::Medium => base.max(150.0),
            QualityTier::Low => base,
        }
    }
}

/// 阈值树（ADR-0007 D3，保守偏向，宁可升级）：
/// 1. 归一化 Laplacian < 0.5 → LOW（明显模糊）
/// 2. 否则 噪声 > 50 或 对比度 < 0.3 → MEDIUM（有污染）
/// 3. 否则 → HIGH
pub fn route(m: &QualityMetrics) -> QualityTier {
    if m.laplacian_norm < 0.5 {
        QualityTier::Low
    } else if m.noise_var > 50.0 || m.contrast < 0.3 {
        QualityTier::Medium
    } else {
        QualityTier::High
    }
}

/// 评估单页质量指标。手写卷积 + 统计。
///
/// 性能：laplacian/gaussian/sobel 三次全图卷积，朴素实现 ~30ms/页@100dpi，前 3 页 <100ms。
pub fn assess(img: &GrayImage) -> QualityMetrics {
    let lap = laplacian_filter(img);
    let blur = gaussian_blur(img, 1.0);
    let sob = sobel_magnitude(img);

    let (w, h) = img.dimensions();
    let n = (w as f32) * (h as f32);

    // 归一化 Laplacian = laplacian 方差 / 原图纹理方差（灰度方差）
    let lap_var = variance_i16(&lap) / n.max(1.0);
    let tex_var = variance_u8(img) / n.max(1.0);
    let laplacian_norm = lap_var / (tex_var + 1e-6);

    // 噪声 = |原图 - 高斯模糊| 的均值
    let noise_var = mean_abs_diff(img, &blur);

    // 对比度 = (max - min) / 255
    let (mut mn, mut mx) = (u8::MAX, u8::MIN);
    for p in img.iter() {
        if *p < mn { mn = *p; }
        if *p > mx { mx = *p; }
    }
    let contrast = (mx as f32 - mn as f32) / 255.0;

    // 平均锐度 = sobel 梯度均值
    let mut s_sum: u64 = 0;
    for v in sob.iter() {
        s_sum += *v as u64;
    }
    let sharpness = (s_sum as f32) / n.max(1.0);

    QualityMetrics { laplacian_norm, noise_var, contrast, sharpness }
}

/// Laplacian 3×3 卷积（kernel `[0,1,0; 1,-4,1; 0,1,0]`），输出 i16 边缘响应。
/// pad 策略：clamp 索引（continuity），与 imageproc `filter3x3` 对齐。
fn laplacian_filter(img: &GrayImage) -> ImageBuffer<Luma<i16>, Vec<i16>> {
    const K: [i16; 9] = [0, 1, 0, 1, -4, 1, 0, 1, 0];
    let (w, h) = img.dimensions();
    let mut out = ImageBuffer::new(w, h);
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let mut acc: i32 = 0;
            for ky in 0..3i32 {
                for kx in 0..3i32 {
                    let px = (x + kx - 1).clamp(0, w as i32 - 1) as u32;
                    let py = (y + ky - 1).clamp(0, h as i32 - 1) as u32;
                    acc += K[(ky * 3 + kx) as usize] as i32 * img.get_pixel(px, py)[0] as i32;
                }
            }
            out.put_pixel(x as u32, y as u32, Luma([acc as i16]));
        }
    }
    out
}

/// Gaussian σ=1 模糊（3×3 近似 kernel `[1,2,1; 2,4,2; 1,2,1] / 16`），输出 u8。
/// σ=1 的 3×3 截断近似足够估计噪声残差；pad 同 `laplacian_filter`。
fn gaussian_blur(img: &GrayImage, _sigma: f32) -> GrayImage {
    const K: [u32; 9] = [1, 2, 1, 2, 4, 2, 1, 2, 1];
    const W_SUM: u32 = 16;
    let (w, h) = img.dimensions();
    let mut out = GrayImage::new(w, h);
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let mut acc: u32 = 0;
            for ky in 0..3i32 {
                for kx in 0..3i32 {
                    let px = (x + kx - 1).clamp(0, w as i32 - 1) as u32;
                    let py = (y + ky - 1).clamp(0, h as i32 - 1) as u32;
                    acc += K[(ky * 3 + kx) as usize] * img.get_pixel(px, py)[0] as u32;
                }
            }
            // 四舍五入
            let v = ((acc + W_SUM / 2) / W_SUM).min(255) as u8;
            out.put_pixel(x as u32, y as u32, Luma([v]));
        }
    }
    out
}

/// Sobel 梯度幅度（L1 范数 |gx|+|gy|），输出 u16。
/// gx = `[-1,0,1; -2,0,2; -1,0,1]`，gy = gx 转置。L1 替代 sqrt(gx²+gy²) 避免浮点，
/// 均值（sharpness）相对量级一致。pad 同上。
fn sobel_magnitude(img: &GrayImage) -> ImageBuffer<Luma<u16>, Vec<u16>> {
    const GX: [i32; 9] = [-1, 0, 1, -2, 0, 2, -1, 0, 1];
    const GY: [i32; 9] = [-1, -2, -1, 0, 0, 0, 1, 2, 1];
    let (w, h) = img.dimensions();
    let mut out = ImageBuffer::new(w, h);
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let mut sx: i32 = 0;
            let mut sy: i32 = 0;
            for ky in 0..3i32 {
                for kx in 0..3i32 {
                    let px = (x + kx - 1).clamp(0, w as i32 - 1) as u32;
                    let py = (y + ky - 1).clamp(0, h as i32 - 1) as u32;
                    let p = img.get_pixel(px, py)[0] as i32;
                    sx += GX[(ky * 3 + kx) as usize] * p;
                    sy += GY[(ky * 3 + kx) as usize] * p;
                }
            }
            let mag = (sx.unsigned_abs() + sy.unsigned_abs()).min(u16::MAX as u32) as u16;
            out.put_pixel(x as u32, y as u32, Luma([mag]));
        }
    }
    out
}

fn variance_i16(img: &image::ImageBuffer<Luma<i16>, Vec<i16>>) -> f32 {
    let mut sum: f64 = 0.0;
    let mut sum_sq: f64 = 0.0;
    let mut n: f64 = 0.0;
    for p in img.iter() {
        let x = *p as f64;
        sum += x;
        sum_sq += x * x;
        n += 1.0;
    }
    if n < 1.0 { return 0.0; }
    let mean = sum / n;
    (sum_sq / n - mean * mean) as f32
}

fn variance_u8(img: &GrayImage) -> f32 {
    let mut sum: f64 = 0.0;
    let mut sum_sq: f64 = 0.0;
    let mut n: f64 = 0.0;
    for p in img.iter() {
        let x = *p as f64;
        sum += x;
        sum_sq += x * x;
        n += 1.0;
    }
    if n < 1.0 { return 0.0; }
    let mean = sum / n;
    (sum_sq / n - mean * mean) as f32
}

fn mean_abs_diff(img: &GrayImage, blur: &GrayImage) -> f32 {
    let mut sum: f64 = 0.0;
    let mut n: f64 = 0.0;
    for (a, b) in img.iter().zip(blur.iter()) {
        sum += (*a as f64 - *b as f64).abs();
        n += 1.0;
    }
    if n < 1.0 { 0.0 } else { (sum / n) as f32 }
}

/// 取前 `n` 页质量的中位数，路由到档位。空集 → HIGH（保守默认清晰）。
pub fn route_pages(imgs: &[GrayImage], n: usize) -> QualityTier {
    let mut metrics: Vec<QualityMetrics> = imgs
        .iter()
        .take(n)
        .map(assess)
        .collect();
    if metrics.is_empty() {
        return QualityTier::High;
    }
    // 中位数：按 laplacian_norm 排序取中（laplacian 是核心模糊指标）
    metrics.sort_by(|a, b| a.laplacian_norm.partial_cmp(&b.laplacian_norm).unwrap_or(std::cmp::Ordering::Equal));
    let mid = &metrics[metrics.len() / 2];
    route(mid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, v: u8) -> GrayImage {
        GrayImage::from_pixel(w, h, Luma([v]))
    }

    #[test]
    fn route_blur_goes_low() {
        // 纯色图：零方差，归一化 Laplacian ≈ 0/<0.5 → LOW
        let m = assess(&solid(100, 100, 128));
        assert_eq!(route(&m), QualityTier::Low);
    }

    #[test]
    fn route_sharp_goes_high() {
        // 棋盘格：高频，Laplacian 方差大
        let mut img = GrayImage::new(100, 100);
        for y in 0..100 {
            for x in 0..100 {
                let v = if (x / 10 + y / 10) % 2 == 0 { 0 } else { 255 };
                img.put_pixel(x, y, Luma([v]));
            }
        }
        let m = assess(&img);
        assert_eq!(route(&m), QualityTier::High, "metrics: {m:?}");
    }

    #[test]
    fn route_noisy_goes_medium() {
        // 高噪声图：逐像素大幅跳变，laplacian 高 + 噪声残差大
        let mut img = GrayImage::new(100, 100);
        for y in 0..100 {
            for x in 0..100 {
                // 伪随机大幅跳变：相邻像素差常 >100
                let v = ((x as u32 * 73 + y as u32 * 137 + (x ^ y) * 11) % 256) as u8;
                img.put_pixel(x, y, Luma([v]));
            }
        }
        let m = assess(&img);
        // 阈值树第一分支守卫：laplacian_norm 须 >= 0.5，否则落 LOW（先于噪声判断）。
        // 跨卷积实现/版本漂移时此前置断言先失败暴露，而非静默落 Low。
        assert!(m.laplacian_norm >= 0.5, "laplacian_norm 应 >=0.5 才能触达噪声分支: {m:?}");
        // 噪声残差 >50 → MEDIUM。固定 == Medium 锁定契约，避免 "Medium||Low" 掩盖回归。
        assert_eq!(route(&m), QualityTier::Medium, "metrics: {m:?}");
    }

    #[test]
    fn route_empty_defaults_high() {
        assert_eq!(route_pages(&[], 3), QualityTier::High);
    }

    #[test]
    fn tier_mapping() {
        assert_eq!(QualityTier::High.tier(), OcrTier::Tiny);
        assert_eq!(QualityTier::Medium.tier(), OcrTier::Tiny);
        assert_eq!(QualityTier::Low.tier(), OcrTier::Small);
        assert_eq!(QualityTier::High.dpi(100.0), 100.0);
        assert_eq!(QualityTier::Medium.dpi(100.0), 150.0);
        assert_eq!(QualityTier::Low.dpi(100.0), 100.0);
    }
}
