//! 总调度（P1.10 统一调度层）：按格式分流到对应通道。
//!
//! 单文档入口 [`convert_to_markdown`] 与 [`crate::batch::BatchConverter`] 共用
//! [`route_doc`] 预分流——文字层快速路径、加密/损坏预检（ADR-0006 §5/§6）、
//! 图片型 PDF 收集进跨文档 OCR pipeline 的判定只此一处；跨文档 pipeline
//! （`pdf::convert_pdf_ocr_docs`）成为 convert 的实现细节，调用方不再感知。
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
    /// 合法区间 `[50, 400]`（审计 #9）：各通道入口统一校验，越界/NaN 直接标错
    /// （CLI 另有 [`crate::validate_render_dpi`] 早失败）。
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
    /// `--pages` 页码选择（仅 PDF；语法见 [`crate::pagerange`]，MinerU/docvortex
    /// 同式：`1-5,8,r3-r1`、`all`）。`None` / `Some("all")` / 空白 = 不限制。
    /// 非 PDF 显式给出 → `Unsupported` 拒绝（MinerU `page_range_invalid` 同口径）。
    pub pages: Option<String>,
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

/// PDF 预分流结论（P1.10）。
pub(crate) enum PdfRoute {
    /// 已出结果：`Ok` = 文字层快速路径命中；`Err` = 加密/损坏，直接标错不送 OCR
    /// （ADR-0006 §5：加密 PDF 送 OCR 也读不了，损坏 PDF 浪费 OCR 资源；
    /// §6：force_ocr 同样不绕过加密预检）。
    Done(Result<String>),
    /// 图片型（无可用文字层）或 force_ocr 强制 → 跨文档 OCR pipeline。
    /// `pages` = `--pages` 选页（`None` = 全页）。
    Ocr { pages: Option<std::collections::BTreeSet<u32>> },
    /// 混合文档（anydoc 0.2.4 缺页上报）：文字层覆盖部分页，`missing_pages`
    /// （1 基）需按页补 OCR 后合并，**不静默丢页**。
    Hybrid {
        /// 文字层 DocIR（跨页表 pass 前）。
        text: crate::docir::DocIR,
        missing_pages: Vec<u32>,
    },
}

/// PDF 预分流（调度层唯一判定处）：classify 元数据闸 → text_layer 探针 →
/// 快速路径 / 混合 / 标错 / OCR。
///
/// `--pages`（借鉴 MinerU → docvortex `page_range.py`，本仓读得其源码、语义
/// 逐条对齐）与页数闸（对齐 MinerU `max_pages_per_file=1000`）共用一次
/// [`crate::pdf::classify_pages`]（不渲图，~10–50ms）：
/// 1. 页数闸恒生效（每 PDF 都拦，`ANYDOC_MAX_PAGES` 可调）；
/// 2. `--pages` 显式给出时求值（rN 换算 / 越界裁剪 / 空集报错），绝对页号下发
///    三条通路——文字层探针只装配所选页、混合只补所选缺页、OCR 只渲所选页；
/// 3. 未给 `--pages` 且页数合规 → 行为与历史逐字节一致（多付一次 classify 元数据）。
pub(crate) fn route_pdf(path: &Path, opts: &ConvertRequest, pdf_force_ocr: bool) -> PdfRoute {
    // 审计 #9：dpi 合法闸（OCR 前置条件，越界直接标错，不跑半途）。
    if let Err(e) = crate::limits::validate_dpi(opts.render.dpi) {
        return PdfRoute::Done(Err(e));
    }
    let total = match pdf::classify_pages(path) {
        Ok(v) => v,
        Err(e) => return PdfRoute::Done(Err(e)),
    };
    if let Err(e) = crate::limits::check_page_count(path, total as u64) {
        return PdfRoute::Done(Err(e));
    }
    // --pages 求值（"all"/空白 = 未给，不限制）。
    let sel = match crate::pagerange::select(opts.pages.as_deref(), total) {
        Ok(s) => s,
        Err(e) => return PdfRoute::Done(Err(e)),
    };
    match pdf::text_layer_probe(path, opts, sel.as_ref()) {
        // 图片型（无可用文字层）→ OCR pipeline
        Ok(None) => PdfRoute::Ocr { pages: sel },
        // force_ocr：丢弃文字层结果整篇送 OCR（图片型校准，行为不变）
        Ok(_) if pdf_force_ocr => PdfRoute::Ocr { pages: sel },
        // 文字层命中且无缺页：快速路径（与旧行为字节一致）
        Ok(Some(pdf::TextHit::Complete(md))) => PdfRoute::Done(Ok(md)),
        // 混合：只补缺页
        Ok(Some(pdf::TextHit::Hybrid { text, missing_pages, .. })) => {
            PdfRoute::Hybrid { text, missing_pages }
        }
        // 加密/损坏 → 直接标错（§5/§6，含 force_ocr 路径的加密预检）
        Err(e) => PdfRoute::Done(Err(e)),
    }
}

/// 文档级预分流结论（P1.10）：单文档入口与 `BatchConverter` 共用。
pub(crate) enum DocRoute {
    /// 已出结果（文字层快速路径 / 探测即失败）
    Done(Result<String>),
    /// 图片型 PDF → 跨文档 OCR pipeline（单文档为 `&[path]` 委托）。
    /// `pages` = `--pages` 选页（`None` = 全页）。
    Ocr { pages: Option<std::collections::BTreeSet<u32>> },
    /// 混合 PDF → 按页补 OCR + 合并（单文档粒度，见 [`PdfRoute::Hybrid`]）
    Hybrid { text: crate::docir::DocIR, missing_pages: Vec<u32> },
    /// 非 PDF 文档 → per-doc 通路（OFD / anydoc 兜底）
    PerDoc(DocKind),
}

/// 统一调度第一步：detect + 大小闸 + `--pages` 适用性 + PDF 文字层预分流。
///
/// P0-3：打不开/读不到返回 `Done(Err(io))`——不再静默归 `Other` 走兜底，
/// 丢失真实 IO 错误分类。
///
/// 审计 #8（口径对齐 MinerU 上传档 413）：一切经调度的输入先过大小闸
/// （[`crate::limits::check_file_size`]，超限 `ResourceLimit` 显式拒绝，不截断
/// 不误读）。放在这一层是因为单文档入口、`BatchConverter` 预分流、库模式
/// 全部经此——一处闸全局生效；dpi 闸则在 route_pdf / convert_ofd 内部。
///
/// `--pages` 仅 PDF 有效（MinerU 对非 PDF 报 `page_range_invalid` 同口径）：
/// 显式给出（非 `all`/空白）且输入非 PDF → `Unsupported` 拒绝。
pub(crate) fn route_doc(path: &Path, opts: &ConvertRequest, force: &ForceFlags) -> DocRoute {
    let kind = match crate::detect::detect(path) {
        Ok(k) => k,
        Err(e) => return DocRoute::Done(Err(ConvertError::io(Stage::Detect, e))),
    };
    if let Err(e) = crate::limits::check_file_size(path) {
        return DocRoute::Done(Err(e));
    }
    match kind {
        DocKind::Pdf => match route_pdf(path, opts, force.pdf_force_ocr) {
            PdfRoute::Done(r) => DocRoute::Done(r),
            PdfRoute::Ocr { pages } => DocRoute::Ocr { pages },
            PdfRoute::Hybrid { text, missing_pages } => DocRoute::Hybrid { text, missing_pages },
        },
        other => {
            if let Some(raw) =
                opts.pages.as_deref().filter(|r| !crate::pagerange::is_unrestricted(r))
            {
                return DocRoute::Done(Err(crate::pagerange::reject_non_pdf(path, raw)));
            }
            DocRoute::PerDoc(other)
        }
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
        // Step 5：HTML 结构化通道（htmd → GFM，对齐 MinerU flash analyze_html）
        DocKind::Html => crate::html::convert_html(path),
        // Step 5：CSV/TSV 分隔文本。anydoc 0.2.4 的 `Format::from_extension` 无 tsv
        // 分支（其 CSV 分隔符嗅探候选含 \t），故 tsv 显式点名 Csv 前端，
        // 对齐 MinerU flash 的 csv/tsv 共用 `analysis/csv.py` 结构化通道。
        DocKind::DelimitedText => convert_delimited(path),
        // Step 5：Office 系（docx/xlsx/rtf/epub…）——anydoc 前端已覆盖，
        // 与旧 `Other` 同路，仅分流归口显式化。
        // P1.9：anydoc 兜底通路错误经 `From<anydoc::ConvertError>` 转入自有类型
        // （kind 分类保留，原始 Display 存 detail）。
        DocKind::Office | DocKind::Other => {
            anydoc::to_markdown(path).map_err(ConvertError::from)
        }
        DocKind::Pdf => pdf::convert_pdf(path, opts, force.pdf_force_ocr),
    }
}

/// CSV/TSV → Markdown 管道表（经 anydoc Csv 前端，含分隔符嗅探）。
fn convert_delimited(path: &Path) -> Result<String> {
    let is_tsv = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("tsv"))
        .unwrap_or(false);
    if !is_tsv {
        return anydoc::to_markdown(path).map_err(ConvertError::from);
    }
    let bytes = std::fs::read(path).map_err(|e| ConvertError::io(Stage::Convert, e))?;
    anydoc::to_markdown_bytes(&bytes, anydoc::Format::Csv).map_err(ConvertError::from)
}

pub fn convert_to_markdown(
    path: &Path,
    opts: &ConvertRequest,
    force: ForceFlags,
) -> Result<String> {
    match route_doc(path, opts, &force) {
        DocRoute::Done(r) => r,
        // 跨文档 pipeline 是 convert 的实现细节：单文档即 `&[path]` 委托
        DocRoute::Ocr { pages } => pdf::convert_pdf_ocr_single(path, opts, pages),
        DocRoute::Hybrid { text, missing_pages } => {
            pdf::convert_pdf_hybrid(path, opts, text, &missing_pages)
        }
        DocRoute::PerDoc(kind) => convert_per_doc(path, kind, opts, &force),
    }
}
