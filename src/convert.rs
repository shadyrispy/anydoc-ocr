//! 总调度（P1.10 统一调度层）：按格式分流到对应通道。
//!
//! 单文档入口 [`convert_to_markdown`] 与 [`crate::batch::BatchConverter`] 共用
//! [`route_doc`] 预分流——文字层快速路径、加密/损坏预检（ADR-0006 §5/§6）、
//! 图片型 PDF 收集进跨文档 OCR pipeline 的判定只此一处；跨文档 pipeline
//! （`pdf::convert_pdf_ocr_docs`）成为 convert 的实现细节，调用方不再感知。
use std::path::Path;

use crate::Result;
use crate::detect::DocKind;
use crate::error::{ConvertError, ErrorKind, Stage};
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
    /// 输出格式（#11）：IR 是真相，格式只是投影。`Default` = markdown
    /// （与本字段引入前逐字节一致）；`ContentListV2` 出 content_list v2 的 JSON，
    /// 仅 PDF / OFD / 图片三种输入支持（其余显式 `Unsupported`，见
    /// [`convert_per_doc`]）。
    pub format: crate::docir::OutputFormat,
}

/// 已废弃环境变量的一次性告警。
///
/// 语义：变量**存在即命中**（不限值）——与它生效时的判据完全一致，这样老脚本
/// `ANYDOC_RICH_TEXT=1`（以及 `=0`、`=`）都会拿到同一句"该变量已废弃且不再改变
/// 行为"，而不是"设了值却什么都没发生"的静默。命中只告警，**不改变任何行为**。
///
/// 只打一次（`OnceLock`，手法同 `models.rs:240`/`ocr_post.rs:421`）：批处理目录
/// 逐文件都过 `route_doc`，不记忆就会每行刷屏。
pub(crate) fn warn_deprecated_env() {
    if !deprecated_env_present_from(std::env::var("ANYDOC_RICH_TEXT").ok().as_deref()) {
        return;
    }
    static NOTICED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    // 文案里的"ANYDOC_RICH_TEXT 已废弃"这段连续文本被 `tests/pages_rich_text.rs`
    // 按出现次数断言（用它数"每进程一次"），改措辞要同步改测试。
    NOTICED.get_or_init(|| {
        eprintln!(
            "[anydoc-ocr] 警告：ANYDOC_RICH_TEXT 已废弃，不再改变任何行为；行内样式改由结构化 span 承载（详见 README 环境变量表）"
        );
    });
}

/// 告警判据（纯函数，可单测）：存在即命中，未设置不命中。
fn deprecated_env_present_from(v: Option<&str>) -> bool {
    v.is_some()
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
    /// `--text-only`（#13，MinerU `--ocr-mode txt` 的**更严**版）：只走文字层，
    /// **一个模型都不加载**（不是"少跑几次 OCR"，是彻底不碰 OCR 通路）。
    ///
    /// 与 `pdf_force_ocr`/`ofd_force_ocr` 同时给出 → `Unsupported` 显式拒绝
    /// （两者语义直接对立，静默取其一比报错更坏）。
    ///
    /// 与 MinerU 的差异（口径要写清，否则对比会误判）：MinerU `txt` 在 medium
    /// 档仍会加载版面/OCR 去处理非文字块（`pipeline.py:54-59`、`ocr.py:38-54`）；
    /// 本开关是给"这台机器不联网、不下模型"和调试用的逃生口，故严格——
    /// 图片型/坏字体文档直接 `needsOcr` 报错，混合文档按 `ANYDOC_NO_HYBRID`
    /// 既有语义出文字层（缺页显式告警列出，不静默）。
    pub text_only: bool,
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
pub(crate) fn route_pdf(path: &Path, opts: &ConvertRequest, force: &ForceFlags) -> PdfRoute {
    // #13：`--text-only` 与 `--pdf-force-ocr` 语义直接对立，同时给出即拒（纯参数
    // 校验，放在任何 IO 之前——不打开文档就能判定，错误也不该随文档内容漂移）。
    if force.text_only && force.pdf_force_ocr {
        return PdfRoute::Done(Err(text_only_conflict(path, "--pdf-force-ocr")));
    }
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
    match pdf::text_layer_probe(path, opts, sel.as_ref(), force.text_only) {
        // 图片型（无可用文字层）→ OCR pipeline；
        // #13 `--text-only`：图片型没有文字层可退，显式 needsOcr 报错（绝不建引擎）。
        Ok(None) if force.text_only => PdfRoute::Done(Err(text_only_needed(path, "整篇无可用文字层（扫描件/坏字体）"))),
        Ok(None) => PdfRoute::Ocr { pages: sel },
        // force_ocr：丢弃文字层结果整篇送 OCR（图片型校准，行为不变）
        Ok(_) if force.pdf_force_ocr => PdfRoute::Ocr { pages: sel },
        // 文字层命中且无缺页：快速路径（与旧行为字节一致）
        Ok(Some(pdf::TextHit::Complete(doc))) => {
            PdfRoute::Done(Ok(crate::docir::finalize(doc, opts.format, false)))
        }
        // 混合：只补缺页；#13 `--text-only` 下按 `ANYDOC_NO_HYBRID` 既有语义出
        // 文字层（缺页丢弃），但**必须显式告警列出页号**——静默丢页正是当初
        // hybrid 路由要修掉的缺陷，这里不是"悄悄降级"。
        Ok(Some(pdf::TextHit::Hybrid { text, missing_pages, .. })) => {
            if force.text_only {
                let mut pages: Vec<String> = missing_pages.iter().map(u32::to_string).collect();
                pages.sort();
                eprintln!(
                    "警告: --text-only 不跑 OCR，{} 的以下页无文字层内容、已按文字层输出（可能缺内容）: {}",
                    path.display(),
                    pages.join(",")
                );
                return PdfRoute::Done(Ok(crate::docir::finalize(text, opts.format, false)));
            }
            PdfRoute::Hybrid { text, missing_pages }
        }
        // 加密/损坏 → 直接标错（§5/§6，含 force_ocr 路径的加密预检）
        Err(e) => PdfRoute::Done(Err(e)),
    }
}

/// #13：`--text-only` 下"本该走 OCR"的显式拒绝（纯函数，可单测）。
///
/// 用 `NeedsOcr`（code=`needsOcr`）而非 `Unsupported`：文档本身没坏、格式也支持，
/// 缺的是"允许我跑 OCR"这一句话——绑定层与 `error_hint` 的既有语义正好接得上。
fn text_only_needed(path: &Path, why: &str) -> ConvertError {
    ConvertError::new(
        ErrorKind::NeedsOcr,
        Stage::Convert,
        format!("--text-only 不跑 OCR，但{}: {}", why, path.display()),
    )
}

/// `--text-only` 与 `--*-force-ocr` 冲突的显式错误（纯函数，可单测）。
fn text_only_conflict(path: &Path, other: &str) -> ConvertError {
    ConvertError::new(
        ErrorKind::Unsupported,
        Stage::Convert,
        format!(
            "--text-only 与 {other} 互斥（前者绝不跑 OCR，后者强制跑）: {}",
            path.display()
        ),
    )
}

/// #12/#13：裸图片输入的 OCR 通道入口（`convert_per_doc` 与单文档共用）。
///
/// 图片没有文字层可言，故 `--text-only` 下直接 `needsOcr` 拒绝——与 PDF 侧
/// "图片型 + text_only → 显式报错"同一条契约，也守住"绝不加载模型"这一半。
fn convert_image(path: &Path, opts: &ConvertRequest, text_only: bool) -> Result<String> {
    if text_only {
        return Err(ConvertError::new(
            ErrorKind::NeedsOcr,
            Stage::Convert,
            format!(
                "图片输入只能 OCR，--text-only 下拒绝处理: {}",
                path.display()
            ),
        ));
    }
    let img = load_image_for_ocr(path)?;
    // #6 第 1 步：图片输入的"页"就是这张位图，PageBox 单位 = 像素。尺寸必须在
    // img 被 move 进 ocr_images 之前取。
    let (w, h) = img.dimensions();
    crate::ocr_engine::init_runtime(&opts.parallel);
    let results = crate::ocr_engine::ocr_images(
        vec![img],
        opts.ocr.tier,
        opts.ocr.layout,
        opts.parallel.page_parallel,
        None,
    )?;
    // #11：IR 是真相 —— 图片输入同样走 DocIR → 按格式投影，不直接产 markdown。
    let doc = crate::gfm_adapter::to_docir(&results, &[Some((w, h))]);
    Ok(crate::docir::finalize(doc, opts.format, true))
}

/// 图片解码 + 像素闸（#12）：任一边超过 [`crate::limits::render_edge_cap`]（默认
/// 3500，对齐 docvortex `DEFAULT_MAX_RENDER_EDGE`）→ **显式** `ResourceLimit` 报错。
///
/// 与 PDF 渲染通路的分工要说清：PDF 侧"长边超钳"是**降 scale 重渲**（渲染参数
/// 是我们自己选的，可以退）；图片的像素是文档自带的，静默缩放=悄悄降质，违反
/// 本仓"超限显式拒绝、绝不静默降质"的硬契约（README 安全闸一行）。要处理大图，
/// 请显式缩放到 `ANYDOC_RENDER_EDGE_CAP` 以内再来。
///
/// 尺寸闸在 `decoder.dimensions()` 上判（**解码前**），故 2 万像素的超大图不会
/// 先把位图分配出来再报错。
///
/// EXIF 朝向：按 `Orientation` 转正后再送 OCR——手机拍的文件照几乎恒带
/// `Orientation != 1`，不转正就是把整页内容侧着喂检测模型（静默精度损失，
/// 比报错严重）。本仓 `--ocr-tier` 各档的 `doc_ori` 方向模型在 mineru 档是关闭的
/// （对齐 MinerU，见 `models::spec_for`），故这里不指望它兜底。
///
/// 多帧（gif）/多页（tiff）只取**首帧**（未使用 `AnimationDecoder::into_frames`
/// 之外的帧）；该语义已在 `--help`/README 注明，不是"待补的多页支持"。
fn load_image_for_ocr(path: &Path) -> Result<image::RgbImage> {
    use image::ImageDecoder as _;
    let reader = image::ImageReader::open(path)
        .map_err(|e| ConvertError::io(Stage::Detect, e))?
        // 格式按魔数判定，不靠扩展名（`-` 入口的临时文件根本没有扩展名）。
        .with_guessed_format()
        .map_err(|e| ConvertError::io(Stage::Detect, e))?;
    let mut decoder = reader
        .into_decoder()
        .map_err(|e| from_image_error(Stage::Detect, e))?;
    // dimensions() 不消费解码器也不失败（头信息已解析）。
    let (w, h) = decoder.dimensions();
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let cap = crate::limits::render_edge_cap();
    let long = w.max(h) as f32;
    if long > cap {
        return Err(ConvertError::new(
            ErrorKind::ResourceLimit,
            Stage::Convert,
            format!(
                "图片长边超限: {} = {w}×{h}px，长边 {long:.0}px > {cap:.0}px（ANYDOC_RENDER_EDGE_CAP 可调；请先等比缩放，本仓不静默降质）",
                path.display()
            ),
        ));
    }
    let mut img = image::DynamicImage::from_decoder(decoder)
        .map_err(|e| from_image_error(Stage::Detect, e))?;
    img.apply_orientation(orientation);
    Ok(img.to_rgb8())
}

/// `image` 错误 → `ConvertError`：`Unsupported`（没有该格式解码器，jp2 即此类：
/// MinerU 认、`image` 0.25 不带 JPEG2000）归 `unsupported`，其余解码类问题与损坏
/// PDF 同类归 `malformed`——都不是运行环境问题（`runtime`）。
fn from_image_error(stage: Stage, e: image::ImageError) -> ConvertError {
    let kind = match &e {
        image::ImageError::Unsupported(_) => ErrorKind::Unsupported,
        _ => ErrorKind::Malformed,
    };
    ConvertError::new(kind, stage, format!("图片解码失败: {e}"))
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
    // 已废弃变量的现场告警放在这一层：单文档 / 批处理 / 库入口都必经此处，
    // 且**与文档类型无关**——废弃变量的持有者不该因为"这次喂的是 OFD"就
    // 拿不到任何反馈。
    warn_deprecated_env();
    let kind = match crate::detect::detect(path) {
        Ok(k) => k,
        Err(e) => return DocRoute::Done(Err(ConvertError::io(Stage::Detect, e))),
    };
    if let Err(e) = crate::limits::check_file_size(path) {
        return DocRoute::Done(Err(e));
    }
    match kind {
        DocKind::Pdf => match route_pdf(path, opts, force) {
            PdfRoute::Done(r) => DocRoute::Done(r),
            PdfRoute::Ocr { pages } => DocRoute::Ocr { pages },
            PdfRoute::Hybrid { text, missing_pages } => DocRoute::Hybrid { text, missing_pages },
        },
        // #12：裸图片（`DocKind::Image`）与其余非 PDF 同走 per-doc——`--pages`
        // 拒绝、大小闸已由上方统一覆盖，差异只在 [`convert_per_doc`] 的通道选择。
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

/// 统一调度第二步：per-doc 通路（图片 / OFD / anydoc 兜底）。
/// `kind` 来自 [`route_doc`]（Pdf 分支不可能到达，防御式兜底重走 PDF 通路）。
pub(crate) fn convert_per_doc(
    path: &Path,
    kind: DocKind,
    opts: &ConvertRequest,
    force: &ForceFlags,
) -> Result<String> {
    // #11：content_list v2 是**结构**投影——只有走 DocIR 的通道（PDF / OFD /
    // 图片）产得出；HTML / CSV / Office 走的是 anydoc 前端直出 markdown，IR 里
    // 没有 bbox 与类型语义，投出来的 content_list 是假的。宁可显式拒绝。
    if opts.format != crate::docir::OutputFormat::Markdown
        && !matches!(kind, DocKind::Ofd | DocKind::Image)
    {
        return Err(ConvertError::new(
            ErrorKind::Unsupported,
            Stage::Convert,
            format!(
                "content_list v2 暂不支持 {} 输入（只有 PDF / OFD / 图片走 DocIR，能给出 bbox 与类型语义）: {}",
                format!("{kind:?}").to_lowercase(),
                path.display()
            ),
        ));
    }
    match kind {
        // #13：OFD 侧与 PDF 侧同一契约——`--text-only` 下不加载任何模型；与
        // `--ofd-force-ocr` 的互斥在 convert_ofd 入口判（各通道入口统一校验惯例）。
        DocKind::Ofd => ofd::convert_ofd(path, opts, force.ofd_force_ocr, force.text_only),
        // #12：裸图片 → 单页 OCR 通道（无渲染、无文字层；text_only 下显式拒绝）。
        DocKind::Image => convert_image(path, opts, force.text_only),
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
        // 防御分支：`route_doc` 的 Pdf 分支不会走到这里（P1.10 起单文档/批处理
        // 同一预分流）。整包透传 force，互斥判定仍在 route_pdf 开头——与本文件
        // 主分支语义严格一致。
        DocKind::Pdf => pdf::convert_pdf(path, opts, force),
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

#[cfg(test)]
mod tests {
    use super::deprecated_env_present_from;

    /// `ANYDOC_RICH_TEXT` 废弃告警的判据：与它生效时的语义**逐字一致**（存在即
    /// 命中，不限值），这样老脚本 `=1` / `=0` / `=` 都会收到同一句"已废弃"，
    /// 而不是设了值却静默无反馈。命中只影响是否打告警，不产生行为差异——
    /// "行为不变"这一半由 `tests/pages_rich_text.rs` 从 CLI 侧钉住。
    #[test]
    fn deprecated_env_presence_matches_legacy_switch_semantics() {
        assert!(!deprecated_env_present_from(None));
        assert!(deprecated_env_present_from(Some("1")));
        assert!(deprecated_env_present_from(Some("0")));
        assert!(deprecated_env_present_from(Some("")));
    }
}
