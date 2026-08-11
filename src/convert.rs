//! 总调度：按格式分流到对应通道
use std::path::Path;

use crate::{Result, detect::DocKind, models::OcrLayout, models::OcrTier, ofd, pdf};

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
        DocKind::Other => anydoc::to_markdown(path).map_err(|e| anyhow::anyhow!("{e}")),
    }
}
