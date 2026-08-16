//! 总调度：按格式分流到对应通道
use std::path::Path;

use crate::Result;
use crate::detect::DocKind;
use crate::{models::OcrLayout, models::OcrTier, ofd, pdf, quality::QualityRoute};

#[derive(Debug, Clone, Default)]
pub struct ConvertOptions {
    /// OCR 模型档
    pub ocr_tier: OcrTier,
    /// 版面模型：默认文档结构 / 表格专用（检出 Table 才跑 SLANet）
    pub ocr_layout: OcrLayout,
    /// OCR 推理线程数
    pub threads: usize,
    /// 渲染 DPI（图片型走 OCR 时的渲染分辨率）
    pub dpi: f32,
    /// ADR-0007：质量路由开关。Auto 渲染前 N 页评估→自动选 tier/dpi；Off 用显式参数
    pub quality_route: QualityRoute,
}

/// 通路私有开关（ADR 候选 4 聚类）：`ofd_force_ocr`/`pdf_force_ocr` 不再混入
/// 公共 `ConvertOptions`，下沉为各 convert 签名显式参数。此处是唯一的显式参数载体，
/// 单文档入口与 `BatchConverter` 持有后透传给对应 convert。
#[derive(Debug, Clone, Copy, Default)]
pub struct ForceFlags {
    /// OFD 强制走 OCR（重建表格结构）
    pub ofd_force_ocr: bool,
    /// PDF 强制走 OCR（文字型 PDF 当图片渲染后 OCR，用于图片型校准）
    pub pdf_force_ocr: bool,
}

pub fn convert_to_markdown(
    path: &Path,
    opts: &ConvertOptions,
    force: ForceFlags,
) -> Result<String> {
    match crate::detect::detect(path) {
        DocKind::Pdf => pdf::convert_pdf(path, opts, force.pdf_force_ocr),
        DocKind::Ofd => ofd::convert_ofd(path, opts, force.ofd_force_ocr),
        // ADR-0006：anydoc 的 ConvertError 直接透传（移除 `map_err(|e| anyhow!("{e}"))`
        // 降级——保留 code() 稳定字符串，调用方可按 code() 精准提示）。
        DocKind::Other => anydoc::to_markdown(path),
    }
}
