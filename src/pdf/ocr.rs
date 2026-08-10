//! OCR 装配：用 oar-ocr 对渲染图做版面+文本+表格分析。
//!
//! 模型构建/缓存已抽到 `crate::ocr_engine::OcrEngine`（按 tier+layout 单例，跨文档复用）。
//! 本模块仅保留 `ocr_images` 兼容调用面，委托 `OcrEngine::predict`。
//! 页级并行（rayon）、零拷贝消费、OCR 页序契约断言均在 `OcrEngine` 内统一实现。
use anyhow::Result;
use image::RgbImage;

use crate::models::{OcrLayout, OcrTier};
use crate::ocr_engine::OcrEngine;

/// 对一组页面图像跑 OCR 管线，返回每页 StructureResult。
///
/// 模型按 `(tier, layout)` 从 `OcrEngine` 单例取/建（首次下载+构建，后续零重载）。
/// `threads` 控制页级并发：多页切成 chunk 用 rayon 并行推理，共享 `Sync` 的分析器。
/// 返回序 = 输入序（页序契约由 `OcrEngine::predict` 断言守恒）。
pub fn ocr_images(
    images: Vec<RgbImage>,
    tier: OcrTier,
    layout: OcrLayout,
    threads: usize,
) -> Result<Vec<oar_ocr::domain::structure::StructureResult>> {
    OcrEngine::build(tier, layout)?.predict(images, threads)
}
