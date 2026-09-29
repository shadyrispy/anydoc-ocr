//! OFD 通道：文字层提取（`text_layer`，P1.7 切分）+ 页型判定（P1.6 集中决策表）
//! + OCR（乱码页批量 / 图片型页流水线，`render`）+ DocIR 装配（P1.5）。
//!
//! 三遍结构（P1.8 拆阶段，每阶段独立函数）：
//! 1. [`classify_pages`]：逐页判定类型（文字层 / F3 乱码页立即渲染 / 图片型页
//!    延迟渲染），信号供 `crate::fallback::decide`（页级粒度）集中裁决；
//! 2. [`probe_route_tier`] + [`ocr_garbled_pages`] + [`ocr_pending_pages`]：
//!    质量路由（ADR-0007 后验置信度门控，只路由 tier）+ 乱码页批量 OCR +
//!    图片型页 render↔OCR 流水线（ADR-0002）；
//! 3. [`assemble_docir`]：DocIR producer 装配 + `docir::passes::cross_page_table`
//!    + 统一渲染。

mod render;
mod text_layer;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use image::RgbImage;
use ofd_core::{OfdReader, RenderOptions};

use crate::ConvertRequest;
use crate::docir::{DocIR, PageDims, PageSource};
use crate::error::{
    ConvertError, ErrorKind, Result as CResult, Stage, from_ofd_error, runtime,
};
use crate::gfm_adapter;
use crate::reading_order;
use crate::region::{Region, RegionKind};
use crate::table_grid;
use crate::timing::StageTimer;
use render::{render_page, try_extract_ofd_page_image};
use text_layer::{collect_text_lines, count_images, is_garbled_text, to_regions};

/// 页型判定阈值：文字总量（字符数）低于该值且存在图像对象时视为图片型页，
/// 走渲染+OCR；否则按坐标提取文字层（与 `--ofd-force-ocr` 无关的默认判定）。
const IMAGE_PAGE_MIN_TEXT_CHARS: usize = 5;

/// 单页数据处理方式（按页序保存，OCR 结果后填）。
///
/// `dims`（#6 第 1 步）：该页 `PageIR.dims` 的值，producer 在第一遍就按
/// **本页区块所在坐标空间**定死——文字层页 = `PhysicalBox`（mm，与
/// `OfdTextLine` 的 `boundary` 同单位），OCR 页 = 送推理的位图（px，见
/// [`OcrPage::dims`]）。拿不到页面框时记 `Unknown`，绝不伪造。
enum PageData {
    /// 纯文字层：坐标行 `Region`（`x_min/x_max/y_min/y_max/文本`）+ 页框（mm）。
    Text(Vec<Region>, PageDims),
    /// F3 坏字体乱码页：第一遍已立即渲染（少量，需保留 fallback 文字层）。
    /// img 用 Option 以便第二遍 `take` 转移所有权，避免双持（T04）。
    OcrFull(Option<RgbImage>),
    /// 图片型页（text_len < 阈值且 img_count > 0）：第一遍**不渲染**，
    /// 记录 (body_idx, page_idx) 待第二遍 P3 流水线渲染+OCR（ADR-0002）。
    /// 全图片型文档的渲染被 OCR 掩盖，峰值内存从 N×页图降到 ~2×页图。
    OcrPendingImage { body_idx: usize, page_idx: usize },
}

/// OCR 页的产出：成品 markdown 段 + 该页 `PageIR.dims`（#6 第 1 步）。
///
/// `dims` 单位固定为 px（送推理的位图宽高）；链路没交出图时记 `Unknown`
/// （`PageDims::default()`），**不**用页框冒充——版面框活在像素空间里。
struct OcrPage {
    md: String,
    dims: PageDims,
}

/// OFD → Markdown 总入口（P1.8 拆阶段）：分类 → 质量路由 → OCR → DocIR 装配。
///
/// `text_only`（#13 `--text-only`）：绝不建 OCR 引擎、不提交 ORT 线程池；需要 OCR
/// 的页按"全篇需 OCR → `needsOcr` 显式报错；部分页需 OCR → 出文字层 + 告警列页号"
/// 处理（与 PDF 侧 `route_pdf` 的 text_only 分支同一条契约，两格式口径一致）。
pub fn convert_ofd(
    path: &Path,
    opts: &ConvertRequest,
    ofd_force_ocr: bool,
    text_only: bool,
) -> CResult<String> {
    // #13：与 `--ofd-force-ocr` 互斥（语义直接对立，静默取其一比报错更坏）。
    if text_only && ofd_force_ocr {
        return Err(ConvertError::new(
            ErrorKind::Unsupported,
            Stage::Convert,
            format!(
                "--text-only 与 --ofd-force-ocr 互斥（前者绝不跑 OCR，后者强制跑）: {}",
                path.display()
            ),
        ));
    }
    // 审计 #9：dpi 合法闸（OFD 走 PerDoc 通路、不经 route_pdf，闸在自家入口）。
    crate::limits::validate_dpi(opts.render.dpi)?;
    let mut t = StageTimer::new();
    // F2：OFD 主路径直接 `OcrEngine::build`（探针/路径 B），不经 `ocr_images`，
    // 须在首个 ONNX session 创建前提交进程级 ORT 线程池（Ticket A）。
    // text_only 下不提交：这条逃生口要能在"只有 pdfium/ofd-core、没有可用模型"
    // 的机器上照常出文字层，尽量少碰运行环境。
    if !text_only {
        crate::ocr_engine::init_runtime(&opts.parallel);
    }
    let mut reader = OfdReader::open(path).map_err(from_ofd_error)?;
    // clone 出来避免遍历时与 reader 的 &mut 借用冲突
    let doc_bodies = reader.ofd().doc_bodies.clone();

    // 第一遍：逐页判定类型（文字层 / F3 乱码页立即渲染 / 图片型页延迟渲染）。
    let mut pages = classify_pages(&mut reader, path, &doc_bodies, opts, ofd_force_ocr)?;

    // 第二遍：OCR（F3 乱码页批量 + 图片型页 P3 流水线），tier 由质量路由决定。
    let ocr_idx: Vec<u32> = pages
        .iter()
        .enumerate()
        .filter(|(_, p)| matches!(p, PageData::OcrFull(_) | PageData::OcrPendingImage { .. }))
        .map(|(i, _)| i as u32)
        .collect();
    let has_ocr_pages = !ocr_idx.is_empty();
    let mut full_out: BTreeMap<u32, OcrPage> = BTreeMap::new();
    if text_only {
        reject_ofd_ocr_pages(path, &ocr_idx, pages.len())?;
    } else {
        let route_tier = probe_route_tier(&mut reader, &doc_bodies, opts, has_ocr_pages);
        full_out = ocr_garbled_pages(&mut pages, route_tier, opts)?;
        full_out.extend(ocr_pending_pages(&pages, path, route_tier, opts, &mut t)?);
    }
    t.stage("gfm");

    // 第三遍：DocIR 装配（跨页表合并 pass + 按格式投影）。
    Ok(crate::docir::finalize(
        assemble_docir(&pages, &mut full_out),
        opts.format,
        false,
    ))
}

/// #6 第 1 步：文字层页的页尺寸 = 该页 `PhysicalBox`（mm，与 `OfdTextLine` 的
/// `boundary` 同单位，见 `text_layer` 头注）。页未声明 `Area` → 回落文档默认
/// `PageArea`；两处都没有 → 记 `Unknown`（**不**按 A4 伪造，与审计 #9 的 dpi
/// 钳位口径不同：那里估算无害，这里伪造会污染归一化分母）。
///
/// `classify_pages` 已把两处的 `Area` 都取到手，故这里只收两个 `Option<&CtPageArea>`
/// （页声明 / 文档默认）而不是一整个 `LoadedDocument`——`Document` 没有 `Default`，
/// 收整个 doc 就没法纯单测这条解析链。审计 #9 的 dpi 钳位走
/// `render::page_physical_box`，那条要自己重新 load_page，与此无关。
fn mm_dims(
    page_area: Option<&ofd_core::model::document::CtPageArea>,
    doc_area: Option<&ofd_core::model::document::CtPageArea>,
) -> PageDims {
    let physical = page_area.map(|a| a.physical_box).or_else(|| doc_area.map(|a| a.physical_box));
    match physical {
        Some(b)
            if b.width > 0.0
                && b.height > 0.0
                && b.width.is_finite()
                && b.height.is_finite() =>
        {
            PageDims::page_box_mm(b.width as f32, b.height as f32)
        }
        // 尺寸非法（0/负/NaN）按"拿不到"处理：记 Unknown，绝不给下游一个假分母。
        _ => PageDims::default(),
    }
}

/// #13：OFD 侧 text_only 的"需 OCR 页"裁决（全篇 → 报错，部分 → 告警）。
///
/// 全篇判定用 `ocr_idx.len() == pages.len()`：一页文字层都没有时，输出空文档
/// 是"假装成功"，不如显式 `needsOcr`（与 PDF 图片型同口径）。
/// 页号按 1 基报出（与告警/错误里给用户的其它页号一致）。
fn reject_ofd_ocr_pages(path: &Path, ocr_idx: &[u32], total_pages: usize) -> CResult<()> {
    if ocr_idx.is_empty() {
        return Ok(());
    }
    if ocr_idx.len() == total_pages {
        return Err(ConvertError::new(
            ErrorKind::NeedsOcr,
            Stage::Convert,
            format!(
                "--text-only 不跑 OCR，但整篇无可用文字层（图片型/坏字体页）: {}",
                path.display()
            ),
        ));
    }
    let listed: Vec<String> = ocr_idx.iter().map(|&i| (i + 1).to_string()).collect();
    eprintln!(
        "警告: --text-only 不跑 OCR，{} 的以下页按图片型/坏字体判定、已按文字层输出（可能缺内容）: {}",
        path.display(),
        listed.join(",")
    );
    Ok(())
}

/// 第一遍：逐页判定类型并收集数据。渲染在循环内完成（需要 per-body `doc`）。
/// 注：原先为"末页强制入可疑集"预扫描过一遍全局总页数，该强制已移除（Ticket B），
/// 预扫描随之删除——省掉一轮跨 doc body 的 load_document。
fn classify_pages(
    reader: &mut OfdReader<std::fs::File>,
    path: &Path,
    doc_bodies: &[ofd_core::model::ofd::DocBody],
    opts: &ConvertRequest,
    ofd_force_ocr: bool,
) -> CResult<Vec<PageData>> {
    let mut pages: Vec<PageData> = Vec::new();
    // 页数闸（对齐 MinerU max_pages_per_file）：OFD 不经 route_pdf，闸在分类循环
    // 内按 body 累计判定，超限在渲染/OCR 之前显式拒绝。多 body 按全文档总页数。
    let mut total_pages: u64 = 0;
    for (body_idx, body) in doc_bodies.iter().enumerate() {
        let doc = reader.load_document(body).map_err(from_ofd_error)?;
        total_pages += doc.pages().len() as u64;
        crate::limits::check_page_count(path, total_pages)?;
        let page_count = doc.pages().len();
        for idx in 0..page_count {
            let page_ref = &doc.pages()[idx];
            // 坏页（尺寸非法/内容缺失等）跳过并告警，而非整体失败——提升对不规范真实 OFD 的健壮性
            let page = match reader.load_page(&doc, page_ref) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("警告: 跳过 OFD 第 {idx} 页（装载失败: {e}）");
                    continue;
                }
            };

            // #6 第 1 步：文字层页的尺寸分母解析链（页 Area → 文档默认 PageArea）。
            let doc_area = doc.document.common_data.page_area.as_ref();
            let texts = collect_text_lines(&page);
            let text_len: usize = texts.iter().map(|line| line.text.chars().count()).sum();
            let img_count = count_images(&page);
            // P1.6：页型判定供信号给集中决策表 [`crate::fallback::decide`]（页级粒度），
            // 本通路不再内联 `text_len < N && img_count > 0` / `is_garbled_text` 路由：
            // - 图片型页 = `BelowCharThreshold` + `ImageObjectPresent` 组合（文字少+有图）；
            // - F3 乱码页 = `GarbledShallow` 单信号（须 >50 字符，与少字信号互斥）。
            let mut signals: Vec<crate::fallback::FallbackSignal> = Vec::new();
            if text_len < IMAGE_PAGE_MIN_TEXT_CHARS {
                signals.push(crate::fallback::FallbackSignal::BelowCharThreshold);
            }
            if img_count > 0 {
                signals.push(crate::fallback::FallbackSignal::ImageObjectPresent);
            }
            if is_garbled_text(&texts) {
                signals.push(crate::fallback::FallbackSignal::GarbledShallow);
            }
            let route = crate::fallback::decide(&signals, crate::fallback::Scope::Page);
            let garbled_f3 = signals.contains(&crate::fallback::FallbackSignal::GarbledShallow);

            if ofd_force_ocr || (route.is_ocr() && !garbled_f3) {
                // P3：图片型页延迟渲染——记录 (body_idx, page_idx) 待第二遍流水线，
                // 不在第一遍立即渲染（避免全图片型文档的 N 次串行渲染不被 OCR 掩盖）。
                pages.push(PageData::OcrPendingImage {
                    body_idx,
                    page_idx: idx,
                });
            } else if route.is_ocr() {
                // F3：坏字体乱码页 → 整页 OCR（渲染失败时回落文字层，不炸文档）
                match render_page(reader, &doc, idx, opts) {
                    // #6 第 1 步：送 OCR 的位图尺寸即该页归一化分母（px），在
                    // take 处现取（见 `ocr_garbled_pages`），这里不必记。
                    Ok(img) => pages.push(PageData::OcrFull(Some(img))),
                    Err(_) => {
                        pages.push(PageData::Text(to_regions(texts), mm_dims(page.area.as_ref(), doc_area)))
                    }
                }
            } else {
                // 双列/多列阅读顺序：复用共享 `reading_order`（PDF 文字层同一算法）。
                // 每行 TextObject 是完整一行，区域直接用其真实页面包围盒
                // （x_min/max、y_min/max 来自 boundary，见 `collect_text_lines`），
                // 使跨整页的页眉/页脚（如"太原市人民政府公报 + 页码"）能命中
                // reading_order 的 is_full 判定，提前到正文之前而非按中心 x 落入
                // 右列；boundary 退化（宽/高非法）时已退回单点区域。
                // Ticket B：移除首/末页"强制入可疑表格页探针集"（无证据召回，代价
                // 是整页渲染+版面 OCR）；表格改由 F1 网格重建（免 OCR）承担，与
                // PDF 侧取舍对称（PDF 另有 probe_last_page_table 兜底末页）。
                pages.push(PageData::Text(to_regions(texts), mm_dims(page.area.as_ref(), doc_area)));
            }
        }
    }
    Ok(pages)
}

/// ADR-0007：OFD 质量路由（后验置信度门控）。仅当有待 OCR 页（OcrFull 乱码页 /
/// OcrPendingImage 图片型页）且 quality_route==Auto 时探针：tiny 渲染+OCR 首 body
/// 首页 → 平均置信度低于阈值 → 升级 small 全篇重跑；否则用显式 opts.ocr.tier。
/// 与 PDF 侧同一算法（needs_upgrade）。**只路由 tier，不改 dpi**：路径 A 的 F3 乱码
/// 页在第一遍循环内已用 opts.render.dpi 渲染（页型判定时同步渲染），改 dpi 会与已渲染图
/// 矛盾；路径 B 走 ADR-0008 直提（downscale 按 dpi×12 控内存），dpi 敏感度低于
/// PDFium 整页光栅化。探针失败（渲染/OCR/装载）回退 opts.ocr.tier，不阻断。
fn probe_route_tier(
    reader: &mut OfdReader<std::fs::File>,
    doc_bodies: &[ofd_core::model::ofd::DocBody],
    opts: &ConvertRequest,
    has_ocr_pages: bool,
) -> crate::models::OcrTier {
    if !(has_ocr_pages && crate::quality::routing_applies(opts)) {
        return opts.ocr.tier;
    }
    const PROBE_DPI: f64 = 100.0;
    let mut needs: Option<bool> = None;
    if let Some(first_body) = doc_bodies.first() {
        if let Ok(doc) = reader.load_document(first_body)
            && let Ok(rgba) = reader.render_page_to_image(&doc, 0, &RenderOptions::with_dpi(PROBE_DPI))
            && let Ok(engine) =
                crate::ocr_engine::OcrEngine::build(crate::models::OcrTier::Tiny, opts.ocr.layout)
            && let Ok(pages_r) = engine.predict(
                vec![image::DynamicImage::ImageRgba8(rgba).to_rgb8()],
                1,
                None,
            )
            && let Some(page) = pages_r.first()
        {
            needs = Some(crate::quality::needs_upgrade(page));
        }
    }
    match needs {
        Some(true) => crate::models::OcrTier::Small,
        Some(false) => crate::models::OcrTier::Tiny,
        None => opts.ocr.tier,
    }
}

/// 路径 A：F3 乱码页批量 OCR（少量，第一遍已渲染 img）。
/// T04：直接 `take` 转移 img 所有权（不 clone），峰值从 2× 降到 1×。
///
/// #6 第 1 步：位图在 `take` 处现取一次宽高（px）随成品段落进 [`OcrPage::dims`]——
/// `ocr_images` 会把图 move 走，事后再问不到。
fn ocr_garbled_pages(
    pages: &mut [PageData],
    route_tier: crate::models::OcrTier,
    opts: &ConvertRequest,
) -> CResult<BTreeMap<u32, OcrPage>> {
    let mut full_pages: Vec<u32> = Vec::new();
    let mut full_imgs: Vec<RgbImage> = Vec::new();
    let mut full_px: Vec<(u32, u32)> = Vec::new();
    for (i, d) in pages.iter_mut().enumerate() {
        if let PageData::OcrFull(img) = d
            && let Some(im) = img.take()
        {
            full_pages.push(i as u32);
            full_px.push((im.width(), im.height()));
            full_imgs.push(im);
        }
    }
    let mut full_out: BTreeMap<u32, OcrPage> = BTreeMap::new();
    if full_imgs.is_empty() {
        return Ok(full_out);
    }
    let timings = crate::timing::PageTimings::new();
    let results = crate::ocr_engine::ocr_images(
        full_imgs,
        route_tier,
        opts.ocr.layout,
        opts.parallel.page_parallel,
        if timings.enabled() {
            Some(&timings)
        } else {
            None
        },
    )?;
    timings.report();
    for ((page, px), res) in full_pages.into_iter().zip(full_px).zip(results) {
        let dims = PageDims::page_box_px(px.0, px.1);
        full_out.insert(
            page,
            OcrPage {
                md: gfm_adapter::to_markdown(std::slice::from_ref(&res), &[Some(px)]),
                dims,
            },
        );
    }
    Ok(full_out)
}

/// OFD 图片型页渲染闭包（T2 复用）：对 `pending` 列表 `(gi, body_idx, page_idx)`
/// 逐页渲染（优先直提 image object，回退整页光栅化），产出 `((0, gi), img)`。
/// 主流程传全部 pending；按页重试传失败页子集——两种路径渲染语义完全一致。
fn ofd_pending_render_fn(
    path: PathBuf,
    pending: Vec<(usize, usize, usize)>,
    dpi: f32,
) -> impl FnOnce(
    std::sync::mpsc::SyncSender<crate::pipeline::RenderItem>,
) -> crate::error::Result<()>
+ Send
+ 'static {
    move |tx: std::sync::mpsc::SyncSender<crate::pipeline::RenderItem>| -> crate::error::Result<()> {
        let mut reader = OfdReader::open(&path).map_err(from_ofd_error)?;
        let bodies = reader.ofd().doc_bodies.clone();
        for (gi, body_idx, page_idx) in &pending {
            let body = &bodies[*body_idx];
            let doc = reader.load_document(body).map_err(from_ofd_error)?;
            // ADR-0008：优先直提 image object（单图满页），跳过整页光栅化。
            if let Some(img) = try_extract_ofd_page_image(&mut reader, &doc, *page_idx, dpi) {
                if tx.send(Ok(((0, *gi), img))).is_err() {
                    break; // OCR 端退出
                }
                continue;
            }
            // 回退整页渲染（混合页/多图块/直提失败）。审计 #9：与 render_page
            // 同闸——按页物理框钳长边（直提路径按原始像素走，dpi 仅控
            // downscale_to_dpi 内存闸，不经此钳位）。
            let eff_dpi = render::page_render_dpi(&mut reader, &doc, *page_idx, dpi);
            match reader.render_page_to_image(&doc, *page_idx, &RenderOptions::with_dpi(eff_dpi.into())) {
                Ok(rgba) => {
                    let img = image::DynamicImage::ImageRgba8(rgba).to_rgb8();
                    if tx.send(Ok(((0, *gi), img))).is_err() {
                        break; // OCR 端退出，停止渲染
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err((
                        (0, *gi),
                        runtime(
                            Stage::Render,
                            Some(*gi),
                            format!("渲染 OFD 页 {gi} 失败: {e}"),
                        ),
                    )));
                }
            }
        }
        Ok(())
    }
}

/// 路径 B：图片型页 P3 流水线——render_fn 闭包内重新 open reader + load + 逐页渲染，
/// 与 OCR 池并发。OfdReader 非 Send → 渲染在专属线程（闭包内 open，不跨线程）。
///
/// #6 第 1 步：位图尺寸由 pipeline 交出（`page_dims`，键 `(0, gi)`），随成品段落进
/// [`OcrPage::dims`]；重试轮的新尺寸覆盖首轮（该页最终用的是重渲染的图）。
fn ocr_pending_pages(
    pages: &[PageData],
    path: &Path,
    route_tier: crate::models::OcrTier,
    opts: &ConvertRequest,
    t: &mut StageTimer,
) -> CResult<BTreeMap<u32, OcrPage>> {
    // (全局页下标 gi, body_idx, page_idx)
    let pending: Vec<(usize, usize, usize)> = pages
        .iter()
        .enumerate()
        .filter_map(|(gi, d)| match d {
            PageData::OcrPendingImage { body_idx, page_idx } => Some((gi, *body_idx, *page_idx)),
            _ => None,
        })
        .collect();
    let mut full_out: BTreeMap<u32, OcrPage> = BTreeMap::new();
    if pending.is_empty() {
        return Ok(full_out);
    }
    t.stage("render");
    let engine = crate::ocr_engine::OcrEngine::build(route_tier, opts.ocr.layout)?;
    let timings = std::sync::Arc::new(crate::timing::PageTimings::new());
    let path = path.to_path_buf();
    let dpi = opts.render.dpi;
    let render_fn = ofd_pending_render_fn(path.clone(), pending.clone(), dpi);
    let (mut results, _render_errors, mut page_dims) = crate::pipeline::PagePipeline::new(
        render_fn,
        engine,
        opts.parallel.page_parallel,
        if timings.enabled() {
            Some(timings.clone())
        } else {
            None
        },
    )
    .run()?;
    t.stage("ocr");
    timings.report();

    // T2：按页失败重试（仅质量路由可用时，见 `quality::routing_applies`）。与 PDF
    // 侧同语义——pipeline 后逐页 `page_needs_retry`，低质量页用更高档局部重跑
    // （重渲染失败页 + 更高档 OCR），成功页保留。只升一档；更高档仍失败/渲染失败
    // 则保留原结果（防循环）。`pending` 提供 gi → (body_idx, page_idx) 映射供重渲染定位。
    if crate::quality::routing_applies(opts) {
        if let Some(higher) = route_tier.next() {
            let by_gi: std::collections::HashMap<usize, (usize, usize)> =
                pending.iter().map(|&(gi, b, p)| (gi, (b, p))).collect();
            let bad: Vec<usize> = results
                .iter()
                .filter(|(_, r)| crate::quality::page_needs_retry(r))
                .map(|((_, gi), _)| *gi)
                .collect();
            if !bad.is_empty() {
                let bad_pending: Vec<(usize, usize, usize)> = bad
                    .iter()
                    .filter_map(|&gi| by_gi.get(&gi).map(|&(b, p)| (gi, b, p)))
                    .collect();
                if !bad_pending.is_empty() {
                    let higher_engine =
                        crate::ocr_engine::OcrEngine::build(higher, opts.ocr.layout)?;
                    let retry_render_fn = ofd_pending_render_fn(path.clone(), bad_pending, dpi);
                    let (retry_results, _retry_errors, retry_dims) = crate::pipeline::PagePipeline::new(
                        retry_render_fn,
                        higher_engine,
                        opts.parallel.page_parallel,
                        if timings.enabled() {
                            Some(timings.clone())
                        } else {
                            None
                        },
                    )
                    .run()?;
                    let retry_map: std::collections::HashMap<usize, _> =
                        retry_results.into_iter().map(|((_, gi), r)| (gi, r)).collect();
                    for ((_, gi), res) in results.iter_mut() {
                        if let Some(new) = retry_map.get(gi) {
                            *res = new.clone();
                            // 该页最终用的是重渲染的图 → 尺寸以重试轮为准。
                            if let Some(px) = retry_dims.get(&(0, *gi)) {
                                page_dims.insert((0, *gi), *px);
                            }
                        }
                    }
                }
            }
        }
    }

    // pipeline 返回 Vec<((doc_idx, page_idx), res)> 按复合键升序。
    // OFD 单文档 doc_idx 恒 0，page_idx = gi 直接映射 full_out；
    // 渲染失败页 gi 缺失 → full_out 无该页 → 第三遍装配跳过（容错）
    for ((_doc_idx, gi), res) in results {
        let px = page_dims.get(&(0, gi)).copied();
        full_out.insert(
            gi as u32,
            OcrPage {
                md: gfm_adapter::to_markdown(std::slice::from_ref(&res), &[px]),
                dims: px.map_or_else(PageDims::default, |(w, h)| PageDims::page_box_px(w, h)),
            },
        );
    }
    Ok(full_out)
}

/// 第三遍：输出装配（P1.5 DocIR producer）。跨页表格（文字层网格，免 OCR）
/// 合并由 `docir::passes::cross_page_table` 承担；图片型 OCR/普通行各自落页。
fn assemble_docir(pages: &[PageData], full_out: &mut BTreeMap<u32, OcrPage>) -> DocIR {
    let mut doc = DocIR::default();
    for (page, data) in pages.iter().enumerate() {
        let page = page as u32;
        match data {
            PageData::OcrFull(_) | PageData::OcrPendingImage { .. } => {
                // 图片型/乱码页：OCR 成品段（gfm_adapter 已产出行 + 表格 HTML 的
                // 最终 markdown），作为 PreRendered 区块原样落页。
                if let Some(op) = full_out.remove(&page) {
                    doc.push_page(
                        page,
                        PageSource::TextLayerOfd,
                        vec![Region::new(0.0, 0.0, 0.0, 0.0, op.md)
                            .with_kind(RegionKind::PreRendered)],
                        op.dims,
                    );
                }
            }
            PageData::Text(lines, pdims) => {
                // 1) F1：文字层网格表（免 OCR、跨页续接）。`reconstruct_table_grid`
                //    内部已做列数/行数/列 x 对齐校验，返回 Some 即"有意义"（列>=2、
                //    行>=2、对齐），单列/参差双列正文自然返回 None 走普通行。
                let page_w = lines.iter().map(|l| l.x_max).fold(0.0_f32, f32::max);
                let blocks: Vec<Region> = lines
                    .iter()
                    .map(|r| {
                        Region::from_top_left(
                            r.x_min,
                            r.y_min,
                            r.x_max - r.x_min,
                            r.y_max - r.y_min,
                            r.text.clone(),
                        )
                    })
                    .collect();
                // 双列正文守卫：OFD 每行 = 左右两个 TextObject（同 y），整行块经
                // `cluster_row` 会被按列间隙拆成 2 列 → `reconstruct_table_grid` 误判
                // 为表格（实测太原公报 6 张"表"全是双列正文）。与 PDF 字符级块不同，
                // 这里必须先用列检测拦截：detect_column_split 检出列 gutter（双列/
                // 多列正文）→ 跳过建表走 reading_order。单列表格页列间隙 <3% 页宽
                // 不触发检测，正常建表。
                let regions: Vec<Region> = lines
                    .iter()
                    .map(|r| Region::new(r.x_min, r.x_max, r.y_min, r.y_max, r.text.clone()))
                    .collect();
                let has_columns = reading_order::detect_column_split(&regions).is_some();
                if !has_columns
                    && let Some(grid) = table_grid::reconstruct_table_grid(&blocks, page_w)
                {
                    // #11b：Grid 块几何 = 参与行的并集（网格横跨这些行的范围）。
                    let gb = blocks.iter().fold(None, |acc, r| {
                        let cur = if r.has_geometry() {
                            Some((r.x_min, r.x_max, r.y_min, r.y_max))
                        } else {
                            None
                        };
                        match (acc, cur) {
                            (None, c) => c,
                            (a, None) => a,
                            (Some((x0, x1, y0, y1)), Some((cx0, cx1, cy0, cy1))) => {
                                Some((x0.min(cx0), x1.max(cx1), y0.min(cy0), y1.max(cy1)))
                            }
                        }
                    })
                    .unwrap_or((0.0, 0.0, 0.0, 0.0));
                    doc.push_page(
                        page,
                        PageSource::TextLayerOfd,
                        vec![Region::new(gb.0, gb.1, gb.2, gb.3, String::new())
                            .with_kind(RegionKind::Grid(grid))],
                        *pdims,
                    );
                    continue;
                }
                // 2) 普通页：文字层行（F4 赋标题级别）为 Body 区块，docir 渲染层
                //    按页 join("\n")（历史无标题空行语义）。#6 第 2 步：`#` 前缀
                //    由渲染层按 `Region.heading_level` 写出，producer 不再拼字面量。
                //    #11b：走 boxed 链路——`lines` 本就带几何（`to_regions` 造的
                //    真实框），此前经 String 薄封装全丢了 → content_list v2 无 bbox。
                //    #11c：追加段落合并（顺序 order → postprocess → merge，理由
                //    与 PDF 文字层同——见 `pdf/text_layer.rs` 尾步注释）。拼接用
                //    拼接与 PDF 文字层同（MinerU 行语境规则，三通路同档）。
                let boxed = reading_order::merge_into_paragraphs(
                    &reading_order::postprocess_lines_boxed(
                        reading_order::order_text_regions_boxed(&regions),
                    ),
                );
                let md: Vec<String> = boxed.iter().map(|l| l.text.clone()).collect();
                let levels = crate::text_health::title_levels(&md, &[], true);
                // #11c-v3 附票：字号补位赋级（同 PDF 文字层，见 pdf/text_layer.rs）
                let sizes: Vec<Option<f32>> = boxed.iter().map(|l| l.font_size).collect();
                let levels = crate::text_health::merge_font_levels(levels, &sizes);
                let out = crate::text_health::body_regions_boxed(boxed, levels);
                doc.push_page(page, PageSource::TextLayerOfd, out, *pdims);
            }
        }
    }
    // #11：返回 **pass 前** 的 DocIR，终渲染（pass + 按格式投影）由调用方做——
    // 原来这里直接 pass + render，投影层就看不到 IR 了。
    doc
}

#[cfg(test)]
mod tests {
    use super::{classify_pages, mm_dims, PageData};
    use crate::docir::{PageDimsKind, PageUnit};
    use ofd_core::{OfdReader, StBox};
    use ofd_core::model::document::CtPageArea;

    fn area(w: f64, h: f64) -> CtPageArea {
        CtPageArea {
            physical_box: StBox::new(0.0, 0.0, w, h),
            application_box: None,
            content_box: None,
            bleed_box: None,
        }
    }

    /// OFD 文字层页的尺寸来源是 `PhysicalBox`（mm），与 `OfdTextLine` 的
    /// `boundary` 同单位，故它是**合法**的归一化分母（与 PDF 侧的
    /// `ContentExtent` 相反，见 `pdf::text_layer` 同名测试）。
    #[test]
    fn ofd_text_layer_dims_are_normalizable_mm_box() {
        let d = mm_dims(Some(&area(210.0, 297.0)), None);
        assert_eq!(d.kind, PageDimsKind::PageBox);
        assert_eq!(d.unit, PageUnit::Mm);
        assert!((d.w - 210.0).abs() < 1e-3 && (d.h - 297.0).abs() < 1e-3);
        assert!(d.normalizable());
    }

    /// 页未声明 `Area` → 回落文档默认 `PageArea`（与 ofd-core 渲染器同一条链）。
    #[test]
    fn ofd_dims_fall_back_to_document_default_area() {
        let d = mm_dims(None, Some(&area(297.0, 210.0)));
        assert!(d.normalizable(), "文档默认框也算真实页面框");
        assert!((d.w - 297.0).abs() < 1e-3 && (d.h - 210.0).abs() < 1e-3);
        // 页声明优先于文档默认。
        let p = mm_dims(Some(&area(210.0, 297.0)), Some(&area(297.0, 210.0)));
        assert!((p.w - 210.0).abs() < 1e-3);
    }

    /// 两处都没有、或尺寸为 0/负/NaN → 记 `Unknown`。
    /// **不**按 A4 伪造（与审计 #9 的 dpi 钳位口径不同：那里 A4 兜底只影响
    /// 渲染分辨率估算，这里伪造会直接污染 bbox 归一化分母）。
    #[test]
    fn ofd_dims_unknown_when_box_unavailable_or_invalid() {
        for case in [
            (None, None),
            (Some(&area(0.0, 297.0)), None),
            (Some(&area(-1.0, 297.0)), None),
            (Some(&area(210.0, f64::NAN)), None),
        ] {
            let d = mm_dims(case.0, case.1);
            assert_eq!(d.kind, PageDimsKind::Unknown, "case {case:?} 应记 Unknown");
            assert_eq!(d.unit, PageUnit::Unknown);
            assert!(!d.normalizable());
        }
    }

    /// **接线路证**（不是 helper 的单测）：走真实 `classify_pages` 通路的 OFD
    /// 文字层页，dims 必须是"该页 PhysicalBox 的 mm 值"。上面三条只证明
    /// `mm_dims` 本身正确，证明不了 producer 真的调了它、也没证明单位没串
    /// ——这里补上（text.ofd 无 OCR，不需要模型环境）。
    #[test]
    fn classify_pages_populates_mm_page_box() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/samples/text.ofd");
        let mut reader = OfdReader::open(&path).expect("open text.ofd");
        let bodies = reader.ofd().doc_bodies.clone();
        let opts = crate::ConvertRequest::default();
        let pages = classify_pages(&mut reader, &path, &bodies, &opts, false).expect("classify");
        assert!(!pages.is_empty(), "text.ofd 应有页");
        let mut seen = 0usize;
        for data in &pages {
            // 本样本是纯文字层文档：任何页都不该落到 OCR 分支。
            let PageData::Text(lines, d) = data else {
                panic!("text.ofd 出现非文字层页（图片型/乱码），测试前提变了");
            };
            assert!(!lines.is_empty());
            assert_eq!(d.kind, PageDimsKind::PageBox);
            assert_eq!(d.unit, PageUnit::Mm, "文字层行与页框同为 mm，单位不得串成 px/pt");
            assert!(d.normalizable(), "OFD 有真实页面框，应可归一化");
            // A4 mm 量级（不是 pt 的 595×842、也不是位图 px）——把单位钉死。
            assert!((100.0..=500.0).contains(&d.w), "页宽应为 mm 量级, got {}", d.w);
            seen += 1;
        }
        assert_eq!(seen, pages.len());
    }
}

