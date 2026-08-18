//! 总调度（P1.10 统一调度层）：按格式分流到对应通道。
//!
//! 单文档入口 [`convert_to_markdown`] 与 [`crate::batch::BatchConverter`] 共用
//! [`route_doc`] 预分流——文字层快速路径、加密/损坏预检（ADR-0006 §5/§6）、
//! 图片型 PDF 收集进跨文档 OCR pipeline 的判定只此一处；跨文档 pipeline
//! （`pdf::convert_pdf_ocr`）成为 convert 的实现细节，调用方不再感知。
use std::path::Path;

use crate::Result;
use crate::detect::DocKind;
use crate::error::{ConvertError, Stage};
use crate::{models::OcrLayout, models::OcrTier, ofd, pdf, quality::QualityRoute};

/// 渲染配置（P2 分层）：文档页 → 位图。
#[derive(Debug, Clone)]
pub struct RenderConfig {
    /// 渲染 DPI（图片型 PDF/OFD 走 OCR 时的渲染分辨率）。
    /// 印刷体公文 100 零精度损失且比 200 快 33%，80 起脚注/小字开始漏检。
    ///
    /// Default = 100.0：修复旧 `ConvertOptions` 的 `dpi=0` 陷阱——0 DPI 渲染出
    /// 空图使 OCR 静默失效，库调用方曾被迫处处显式设 dpi。
    pub dpi: f32,
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self { dpi: 100.0 }
    }
}

/// OCR 配置（P2 分层）：模型档与版面模型。
#[derive(Debug, Clone, Default)]
pub struct OcrConfig {
    /// OCR 模型档
    pub tier: OcrTier,
    /// 版面模型：默认文档结构 / 表格专用（检出 Table 才跑 SLANet）
    pub layout: OcrLayout,
}

/// 并行配置（P2 分层）：旧 `threads` 单字段双语义拆开。
///
/// - `page_parallel`：渲染↔OCR pipeline 的**页级**并发数；
/// - `ort_intra`：单次 ORT 推理 run **内**的 intra-op 线程数。
///
/// 二者相乘≈总线程数；`ort_intra=0` 表示自动取 `max(1, cores/page_parallel)`
/// （[`crate::ocr_engine::init_runtime`]），使总线程≈核心数、无超额订阅。
#[derive(Debug, Clone)]
pub struct ParallelConfig {
    /// 页级并行度（渲染↔OCR pipeline 的页级并发数）
    pub page_parallel: usize,
    /// ORT intra-op 线程数；0 = 自动（cores/page_parallel，env 可调试覆盖）
    pub ort_intra: usize,
}

impl Default for ParallelConfig {
    /// 页级并行默认取可用并行度（飞腾 D2000 8 核→8）；内存受限环境可显式调小。
    fn default() -> Self {
        let page_parallel = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        Self { page_parallel, ort_intra: 0 }
    }
}

/// 转换请求（P2 配置分层）：render / ocr / parallel 三层 + 质量路由。
///
/// 取代旧扁平 `ConvertOptions{ocr_tier, ocr_layout, threads, dpi, ...}`——
/// `threads` 的"页级并行 / ORT 池内"双语义已拆入 [`ParallelConfig`]，
/// `dpi` 的 `Default=0` 陷阱已修（[`RenderConfig`] 默认 100.0）。
#[derive(Debug, Clone, Default)]
pub struct ConvertRequest {
    /// 渲染参数（DPI 等）
    pub render: RenderConfig,
    /// OCR 模型参数（档位 / 版面模型）
    pub ocr: OcrConfig,
    /// 并行参数（页级并行 / ORT intra-op）
    pub parallel: ParallelConfig,
    /// ADR-0007：质量路由开关。Auto 渲染前 N 页评估→自动选 tier/dpi；Off 用显式参数
    pub quality_route: QualityRoute,
}

/// 通路私有开关（ADR 候选 4 聚类）：`ofd_force_ocr`/`pdf_force_ocr` 不再混入
/// 公共 `ConvertRequest`，下沉为各 convert 签名显式参数。此处是唯一的显式参数载体，
/// 单文档入口与 `BatchConverter` 持有后透传给对应 convert。
#[derive(Debug, Clone, Copy, Default)]
pub struct ForceFlags {
    /// OFD 强制走 OCR（重建表格结构）
    pub ofd_force_ocr: bool,
    /// PDF 强制走 OCR（文字型 PDF 当图片渲染后 OCR，用于图片型校准）
    pub pdf_force_ocr: bool,
}

/// PDF 预分流结论（P1.10）：文字层探针后的去向。
pub(crate) enum PdfRoute {
    /// 已出结果：`Ok` = 文字层快速路径命中；`Err` = 加密/损坏，直接标错不送 OCR
    /// （ADR-0006 §5：加密 PDF 送 OCR 也读不了，损坏 PDF 浪费 OCR 资源；
    /// §6：force_ocr 同样不绕过加密预检）。
    Done(Result<String>),
    /// 图片型（无可用文字层）或 force_ocr 强制 → 跨文档 OCR pipeline。
    Ocr,
}

/// PDF 预分流（调度层唯一判定处）：text_layer 探针 → 快速路径 / 标错 / OCR。
pub(crate) fn route_pdf(path: &Path, opts: &ConvertRequest, pdf_force_ocr: bool) -> PdfRoute {
    match pdf::text_layer_markdown(path, opts) {
        // 图片型（无可用文字层）→ OCR pipeline
        Ok(None) => PdfRoute::Ocr,
        // 文字层命中：force_ocr 丢弃文字层结果送 OCR（图片型校准），否则快速路径出结果
        Ok(Some(md)) if !pdf_force_ocr => PdfRoute::Done(Ok(md)),
        Ok(Some(_)) => PdfRoute::Ocr,
        // 加密/损坏 → 直接标错（§5/§6，含 force_ocr 路径的加密预检）
        Err(e) => PdfRoute::Done(Err(e)),
    }
}

/// 文档级预分流结论（P1.10）：单文档入口与 `BatchConverter` 共用。
pub(crate) enum DocRoute {
    /// 已出结果（文字层快速路径 / 探测即失败）
    Done(Result<String>),
    /// 图片型 PDF → 跨文档 OCR pipeline（单文档为 `&[path]` 委托）
    Ocr,
    /// 非 PDF 文档 → per-doc 通路（OFD / anydoc 兜底）
    PerDoc(DocKind),
}

/// 统一调度第一步：detect + PDF 文字层预分流。
///
/// P0-3：打不开/读不到返回 `Done(Err(io))`——不再静默归 `Other` 走兜底，
/// 丢失真实 IO 错误分类。
pub(crate) fn route_doc(path: &Path, opts: &ConvertRequest, force: &ForceFlags) -> DocRoute {
    let kind = match crate::detect::detect(path) {
        Ok(k) => k,
        Err(e) => return DocRoute::Done(Err(ConvertError::io(Stage::Detect, e))),
    };
    match kind {
        DocKind::Pdf => match route_pdf(path, opts, force.pdf_force_ocr) {
            PdfRoute::Done(r) => DocRoute::Done(r),
            PdfRoute::Ocr => DocRoute::Ocr,
        },
        other => DocRoute::PerDoc(other),
    }
}

/// 统一调度第二步：per-doc 通路（OFD / anydoc 兜底）。
/// `kind` 来自 [`route_doc`]（Pdf 分支不可能到达，防御式兜底重走 PDF 通路）。
pub(crate) fn convert_per_doc(
    path: &Path,
    kind: DocKind,
    opts: &ConvertRequest,
    force: &ForceFlags,
) -> Result<String> {
    match kind {
        DocKind::Ofd => ofd::convert_ofd(path, opts, force.ofd_force_ocr),
        // P1.9：anydoc 兜底通路错误经 `From<anydoc::ConvertError>` 转入自有类型
        // （kind 分类保留，原始 Display 存 detail）。
        DocKind::Other => anydoc::to_markdown(path).map_err(ConvertError::from),
        DocKind::Pdf => pdf::convert_pdf(path, opts, force.pdf_force_ocr),
    }
}

pub fn convert_to_markdown(
    path: &Path,
    opts: &ConvertRequest,
    force: ForceFlags,
) -> Result<String> {
    match route_doc(path, opts, &force) {
        DocRoute::Done(r) => r,
        // 跨文档 pipeline 是 convert 的实现细节：单文档即 `&[path]` 委托
        DocRoute::Ocr => pdf::convert_pdf_ocr_single(path, opts),
        DocRoute::PerDoc(kind) => convert_per_doc(path, kind, opts, &force),
    }
}
