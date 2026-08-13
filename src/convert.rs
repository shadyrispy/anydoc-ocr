//! 总调度：按格式分流到对应通道
use std::path::Path;

use image::RgbImage;

use crate::{
    detect::DocKind, gfm_adapter, models::OcrLayout, models::OcrTier, ocr, ofd, pdf, Result,
};

#[derive(Debug, Clone, Default)]
pub struct ConvertOptions {
    /// OCR 模型档
    pub ocr_tier: OcrTier,
    /// 版面模型：默认文档结构 / 表格专用（检出 Table 才跑 SLANet）
    pub ocr_layout: OcrLayout,
    /// OFD 强制走 OCR（重建表格结构）
    pub ofd_force_ocr: bool,
    /// PDF 强制走 OCR（文字型 PDF 当图片渲染后 OCR，用于图片型校准）
    pub pdf_force_ocr: bool,
    /// OCR 推理线程数
    pub threads: usize,
    /// 渲染 DPI（图片型走 OCR 时的渲染分辨率）
    pub dpi: f32,
}

pub fn convert_to_markdown(path: &Path, opts: &ConvertOptions) -> Result<String> {
    match crate::detect::detect(path) {
        DocKind::Pdf => pdf::convert_pdf(path, opts),
        DocKind::Ofd => ofd::convert_ofd(path, opts),
        DocKind::Other => convert_with_anydoc(path, opts),
    }
}

/// anydoc 通道：doc/docx/xls/xlsx/ppt/rtf/odt/csv 等文本格式。
///
/// 文本主体走 `to_markdown_bytes`（anydoc `render` 模块私有，无法直接复用
/// `document_to_markdown`，阶段3 自写渲染器后消除该二次 parse）。嵌入图片资产
/// 经 `to_document` 取出，解码后送共用 OCR 管线，结果追加到输出末尾。
/// assets 为空时早返回，避免无意义初始化 OCR 引擎。
fn convert_with_anydoc(path: &Path, opts: &ConvertOptions) -> Result<String> {
    let bytes = std::fs::read(path)?;

    // 文本主体：to_markdown_bytes 内部对非 PDF 格式 = document_to_markdown(to_document(...))
    let mut md = anydoc::to_markdown_bytes(&bytes, None)?;

    // 资产消费：to_document 取 assets，image/* 解码后批量 OCR
    let doc = anydoc::to_document(&bytes, None)?;
    if doc.assets.is_empty() {
        return Ok(md);
    }

    let mut images: Vec<RgbImage> = Vec::new();
    for asset in &doc.assets {
        if !asset.media_type.starts_with("image/") {
            continue;
        }
        match image::load_from_memory(&asset.bytes) {
            Ok(img) => images.push(img.to_rgb8()),
            Err(e) => eprintln!(
                "警告: 跳过资产 #{}（{} 解码失败: {e}）",
                asset.id.0, asset.media_type
            ),
        }
    }
    if images.is_empty() {
        return Ok(md);
    }

    // OCR 失败(模型下载/推理)不阻断已成功的文本主体：降级告警返回文本
    let ocr_md = match ocr::ocr_images(images, opts.ocr_tier, opts.ocr_layout, opts.threads) {
        Ok(results) => gfm_adapter::structure_results_to_gfm(&results),
        Err(e) => {
            eprintln!("警告: 资产 OCR 失败，跳过图片文本: {e}");
            String::new()
        }
    };
    if !ocr_md.trim().is_empty() {
        md.push_str("\n\n");
        md.push_str(&ocr_md);
    }
    Ok(md)
}
