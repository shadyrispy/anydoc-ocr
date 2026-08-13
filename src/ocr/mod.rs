//! OCR 后端抽象：trait 隔离引擎实现，支持可插拔（ORT/NCNN）。
//!
//! 当前仅 ORT 后端（封装 oar-ocr）。NCNN 后端预留 feature gate，用户可对接
//! 已转换的 NCNN 模型获得 ARM 平台 2.5-3x 加速。
pub mod ort;

pub use ort::OrtEngine;

use crate::models::{ModelSpec, OcrLayout, OcrTier};
use image::RgbImage;

/// OCR 引擎抽象：批量预测页面图像，返回每页结构化结果。
/// 实现需 Send+Sync 以支持 rayon 并行。
pub trait OcrEngine: Send + Sync {
    fn predict(&self, images: &[RgbImage]) -> crate::Result<Vec<oar_ocr::domain::structure::StructureResult>>;
}

/// OCR 后端类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OcrBackend {
    /// ONNX Runtime（现状，x86 默认）
    #[default]
    Ort,
    /// NCNN（飞腾 ARM 加速，预留接口）
    Ncnn,
}

/// 按规格与后端构造引擎
pub fn build_engine(
    spec: &ModelSpec,
    layout: OcrLayout,
    threads: usize,
    backend: OcrBackend,
) -> crate::Result<Box<dyn OcrEngine>> {
    match backend {
        OcrBackend::Ort => Ok(Box::new(OrtEngine::new(spec, layout, threads)?)),
        OcrBackend::Ncnn => {
            #[cfg(not(feature = "ncnn"))]
            {
                let _ = (spec, layout, threads);
                Err(crate::Error::Unsupported(
                    "NCNN 后端未启用：编译时加 --features ncnn".into(),
                ))
            }
            #[cfg(feature = "ncnn")]
            {
                Err(crate::Error::Unsupported(
                    "NCNN 后端 feature 已启用但实现未提供（TODO: 对接 NcnnEngine）".into(),
                ))
            }
        }
    }
}

/// 便利函数：按 tier/layout/threads/backend 构造引擎（内部解析 spec）
pub fn build_engine_tier(
    tier: OcrTier,
    layout: OcrLayout,
    threads: usize,
    backend: OcrBackend,
) -> crate::Result<Box<dyn OcrEngine>> {
    let spec = crate::models::spec_for(tier);
    build_engine(&spec, layout, threads, backend)
}

/// 对一组页面图像跑 OCR 管线，返回每页 StructureResult。
///
/// PDF/OFD 图片型通道共用入口。模型按 `tier` 选，首次经 auto-download 从
/// ModelScope 拉取（缓存 $OAR_HOME）。`layout=Table` 换表格专用版面
/// （`picodet_layout_1x_table.onnx`，只标 Table，检出才跑 SLANet）。
/// `threads` 控制**页级并发**：多页切 chunk，rayon 并行 `predict_images`；
/// 单页或 `threads<=1` 走原生调用（保留 oar-ocr 内部 batching）。
/// 注：oar-ocr/ORT intra-op 线程池不可外部调参，本旋钮控制"同时推理的页数"。
pub fn ocr_images(
    images: Vec<RgbImage>,
    tier: OcrTier,
    layout: OcrLayout,
    threads: usize,
) -> crate::Result<Vec<oar_ocr::domain::structure::StructureResult>> {
    if images.is_empty() {
        return Ok(Vec::new());
    }
    // 阶段2.2：固定 ORT 后端（NCNN 待 feature 启用后由调用方选）
    let engine = build_engine_tier(tier, layout, threads, OcrBackend::Ort)?;
    engine.predict(&images)
}
