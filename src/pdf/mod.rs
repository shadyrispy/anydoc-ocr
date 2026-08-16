//! PDF 通道：文字型走 pdf-inspector 提取 + 自建阅读顺序还原；图片型走 OCR 管线。
//!
//! 文字型不再用 `anydoc::to_markdown()`：其内部是 pdf-inspector 的"既有版式路径"
//! （朴素 y/x 排序），双列页面逐行交错，且 pdf-inspector 内置 reading_order 是
//! 图像锚定、证据门控的局部列流处理，纯文字双列页永不触发。改为直接调
//! `extract_text_with_positions()` 拿带坐标 TextItem → 复用公共模块
//! `reading_order`（与 OCR 通路同一算法）还原阅读顺序。
//!
//! 关键：pdf-inspector 的 `group_into_lines` 会把同一行的左右两列合并成一行
//! （先于列检测糊掉列边界），所以这里在检测到双列后**按 gutter 拆行**，把每行
//! 拆成列内独立行，再交给 `order_text_regions` 做左列全→右列全。
//!
//! 文字层提取管线（`text_layer_markdown` 及其辅助函数/常量/测试）已拆至
//! 独立文件 [`text_layer`](self::text_layer)。
//!
//! T2-B/R1/R3：文字层通路对"含表格页"回退 OCR。不再单靠 pdf-inspector 的
//! `pages_with_tables`（弱：漏首页标题块、偶误报），改为可疑集 = 文字层启发式
//! （>=3 行各自拆成 >=3 个 x 分离段，双列正文每行仅 2 段不误报）；首/末页
//! 曾无条件入集，Ticket B 已移除（无证据召回，代价是整页渲染+版面 OCR），末页
//! 表格改由 `probe_last_page_table` 兜底。可疑页整文档懒渲染一次后批量跑版面
//! OCR（用 `opts.ocr_layout`，默认 Doc 含 table 类，能识别封面/版权栏等），
//! 以 `LayoutElementType::Table` 确认后才输出 `<table>` HTML（MinerU 对齐：
//! 表格只出自识别模型，不来自文字层），未确认页回落文字层；页序混排保序，
//! OCR 失败回落该页文字层。`--pdf-force-ocr` 仍为整文档 OCR。
//!
//! T2-B：跨页重复的"页面家具/水印"（页眉/页脚/居中/斜向水印）在文字层按
//! "同文本 + 同归一化位置跨页重复达阈值"剔除，避免污染阅读顺序。
//!
//! ADR-0005 候选 2：`convert_pdf_ocr` 接受 `&[PathBuf]`，跨文档 render↔OCR
//! pipeline（[`render::render_cross_doc_fn`]）。`convert_pdf` 单文档调用方
//! 委托给它（`&[path]`）。`BatchConverter::convert_many` 预分流：文字型 doc
//! 走 `text_layer_markdown` 快速路径，图片型 doc 收集到 `ocr_paths` 一次性
//! 送入 `convert_pdf_ocr`——文档边界 OCR 池空转消除 + 小文档 setup 摊薄。
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::timing::StageTimer;
use crate::{ConvertOptions, Result, gfm_adapter};

pub mod render;
mod text_layer;
pub(crate) use text_layer::text_layer_markdown;

pub fn convert_pdf(path: &Path, opts: &ConvertOptions) -> Result<String> {
    let mut t = StageTimer::new();
    // 文字型：pdf-inspector 提取 + 自建阅读顺序；非文字型/失败回退 OCR。
    // --pdf-force-ocr 强制把文字型当图片渲染后 OCR（图片型校准）。
    if !opts.pdf_force_ocr
        && let Some(md) = text_layer_markdown(path, opts)?
    {
        return Ok(md);
    }
    // 图片型：跨文档 OCR pipeline（ADR-0005 候选 2）。单文档委托为 &[path]。
    let path_buf = path.to_path_buf();
    let mut out = convert_pdf_ocr(std::slice::from_ref(&path_buf), opts)?;
    t.stage("ocr"); // render 已被 OCR 掩盖，合并记为 ocr
    // 单文档：唯一 doc 的 Result 直接透传（Err 能带真实 detail，ADR 候选 3）。
    match out.pop().map(|(_, r)| r) {
        Some(Ok(md)) => Ok(md),
        Some(Err(e)) => Err(e),
        None => Ok(String::new()),
    }
}

/// 跨文档图片型 PDF OCR（ADR-0005 候选 2）。
///
/// 入参 `paths` 为预分流后的图片型 PDF 列表（已确认无可用文字层，或
/// `pdf_force_ocr` 强制）。返回 `Vec<(doc_idx, Result<String>)>` 按 doc_idx 升序——
/// doc_idx 与入参 paths 的索引一一对应，调用方据此回填每文档的 Result。
/// 每文档独立 Result：整文档打开失败/全页渲染失败 → 该 doc_idx 对应 Err（带真实
/// detail，ADR 候选 3），其它文档不受影响（错误隔离）。
///
/// 跨文档 render 闭包逐 doc open + 逐页渲染，OCR 池跨文档消费，文档边界不停顿。
/// pipeline 返回 `(成功页, 渲染错误)` 按复合键升序，此处按 doc_idx 分组（同组内
/// page_idx 升序）逐 doc 跑 `structure_results_to_gfm`。
pub(crate) fn convert_pdf_ocr(
    paths: &[PathBuf],
    opts: &ConvertOptions,
) -> Result<Vec<(usize, Result<String>)>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    // ADR-0007：质量路由（后验置信度门控）。Auto 时 tiny 渲染+OCR 首文档首页，
    // 平均置信度低于阈值 → 升级 small 全篇重跑；Off 用显式 opts.ocr_tier。
    // 探针失败不阻断，回退显式参数。dpi 始终由用户显式控制（后验只升级 tier，
    // 不再改 dpi）。
    let tier = if opts.quality_route == crate::quality::QualityRoute::Auto {
        probe_first_doc_confidence(&paths[0], opts)?
            .map(|needs| if needs { crate::models::OcrTier::Small } else { crate::models::OcrTier::Tiny })
            .unwrap_or(opts.ocr_tier)
    } else {
        opts.ocr_tier
    };
    let dpi = opts.dpi;
    let engine = crate::ocr_engine::OcrEngine::build(tier, opts.ocr_layout)?;
    let timings = std::sync::Arc::new(crate::timing::PageTimings::new());
    let render_fn = render::render_cross_doc_fn(paths.to_vec(), dpi);
    let (results, render_errors) = crate::pipeline::PagePipeline::new(
        render_fn,
        engine,
        opts.threads,
        if timings.enabled() { Some(timings.clone()) } else { None },
    )
    .run()?;
    timings.report();

    // 按复合键 (doc_idx, page_idx) 升序结果分组——pipeline 已保证页序，
    // 同 doc_idx 组内 page_idx 升序，直接 collect 进 Vec 保序。
    let mut by_doc: BTreeMap<usize, Vec<oar_ocr::domain::structure::StructureResult>> =
        BTreeMap::new();
    for ((doc_idx, _page_idx), res) in results {
        by_doc.entry(doc_idx).or_default().push(res);
    }
    // 整文档失败（哨兵页 usize::MAX 标记打开失败）→ 该 doc_idx 标 Err（带真实 detail）。
    // 单页失败（page < usize::MAX）不标错——该 doc 其余页仍产出，保错误隔离。
    let mut doc_errors: BTreeMap<usize, crate::error::ConvertError> = BTreeMap::new();
    for ((doc_idx, page_idx), e) in render_errors {
        if page_idx == usize::MAX {
            doc_errors.insert(doc_idx, e);
        }
    }
    let mut out = Vec::with_capacity(paths.len());
    for (doc_idx, pages) in by_doc {
        if let Some(e) = doc_errors.remove(&doc_idx) {
            out.push((doc_idx, Err(e)));
        } else {
            let md = gfm_adapter::structure_results_to_gfm(&pages);
            out.push((doc_idx, Ok(md)));
        }
    }
    // 整批路径中无任何成功页的 doc（打开失败）——已由 doc_errors 覆盖；若
    // 仍有 doc_idx 完全缺失但无错误（理论不出现），补兜底 Err。
    for (doc_idx, e) in doc_errors {
        out.push((doc_idx, Err(e)));
    }
    out.sort_by_key(|(i, _)| *i);
    Ok(out)
}

/// ADR-0007（后验）：tiny 渲染+OCR 首文档首页 → 平均置信度 → 是否升级 small。
/// 返回 `Some(true)` 升级、`Some(false)` 维持 tiny；探针失败（渲染/OCR）返回 Ok(None)，
/// 调用方回退显式参数。探针仅 1 页 tiny，开销最小。
fn probe_first_doc_confidence(path: &Path, opts: &ConvertOptions) -> Result<Option<bool>> {
    let imgs = render::render_pdf_pages(path, opts.dpi, &[0])?;
    // 探针固定用 tiny：本就是要判定 tiny 是否够用
    let engine = crate::ocr_engine::OcrEngine::build(crate::models::OcrTier::Tiny, opts.ocr_layout)?;
    let pages = engine.predict(imgs, 1, None)?;
    let Some(page) = pages.first() else {
        return Ok(None);
    };
    Ok(Some(crate::quality::needs_upgrade(page)))
}
