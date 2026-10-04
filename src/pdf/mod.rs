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
//! OCR（用 `opts.ocr.layout`，默认 Doc 含 table 类，能识别封面/版权栏等），
//! 以 `LayoutElementType::Table` 确认后才输出 `<table>` HTML（MinerU 对齐：
//! 表格只出自识别模型，不来自文字层），未确认页回落文字层；页序混排保序，
//! OCR 失败回落该页文字层。`--pdf-force-ocr` 仍为整文档 OCR。
//!
//! T2-B：跨页重复的"页面家具/水印"（页眉/页脚/居中/斜向水印）在文字层按
//! "同文本 + 同归一化位置跨页重复达阈值"剔除，避免污染阅读顺序。
//!
//! ADR-0005 候选 2：`convert_pdf_ocr_docs` 接受跨文档规格列表，跨文档 render↔OCR
//! pipeline（[`render::render_cross_doc_pages_fn`]）。`convert_pdf` 单文档调用方
//! 委托给它（单元素规格）。`BatchConverter::convert_many` 预分流：文字型 doc
//! 走 `text_layer_probe` 快速路径，图片型/混合型 doc 收集到同一批 OCR 规格一次性
//! 送入 `convert_pdf_ocr_docs`——文档边界 OCR 池空转消除 + 小文档 setup 摊薄。
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::docir::DocIR;
use crate::timing::StageTimer;
use crate::{ConvertRequest, Result, gfm_adapter};

pub mod render;
mod text_layer;
pub(crate) use text_layer::{TextHit, classify_pages, text_layer_probe};

mod page_box;

pub fn convert_pdf(
    path: &Path,
    opts: &ConvertRequest,
    force: &crate::convert::ForceFlags,
) -> Result<String> {
    let mut t = StageTimer::new();
    // P1.10：文字层预分流统一走 convert::route_pdf（单文档与 BatchConverter
    // 共用同一判定；force_ocr 路径含加密预检 ADR-0006 §6）。
    match crate::convert::route_pdf(path, opts, force) {
        crate::convert::PdfRoute::Done(r) => r,
        crate::convert::PdfRoute::Ocr { pages } => {
            t.stage("ocr"); // render 已被 OCR 掩盖，合并记为 ocr
            convert_pdf_ocr_single(path, opts, pages)
        }
        crate::convert::PdfRoute::Hybrid { text, missing_pages, .. } => {
            t.stage("hybrid");
            convert_pdf_hybrid(path, opts, text, &missing_pages)
        }
    }
}

/// 已完成预分流（Ocr 路由）的单文档入口：直接进跨文档 pipeline（`&[path]` 委托）。
///
/// P1.10：供 `convert_to_markdown` 的 `DocRoute::Ocr` 分支调用——跨文档 pipeline
/// 是 convert 的实现细节，调用方不再自行组 `&[path]`。
/// `pages` = `--pages` 选页（`None` = 全页；Some 时仅渲染/识别所选页）。
pub(crate) fn convert_pdf_ocr_single(
    path: &Path,
    opts: &ConvertRequest,
    pages: Option<std::collections::BTreeSet<u32>>,
) -> Result<String> {
    let spec = match pages {
        None => OcrDocSpec::scan(path.to_path_buf()),
        Some(sel) => OcrDocSpec {
            path: path.to_path_buf(),
            missing_pages: Some(sel.into_iter().collect()),
            text: None,
        },
    };
    let mut out = convert_pdf_ocr_docs(vec![spec], opts)?;
    // 单文档：唯一 doc 的 Result 直接透传（Err 能带真实 detail，ADR 候选 3）。
    match out.pop().map(|(_, r)| r) {
        Some(r) => r,
        None => Ok(String::new()),
    }
}

/// 混合 PDF 单文档入口（anydoc 0.2.4 缺页上报 → 只补缺页）：文字层已覆盖
/// 大部分页，仅 `missing_pages`（1 基）进 OCR pipeline，再按页号合并。
///
/// 与批处理共用 [`convert_pdf_ocr_docs`]（同一 pipeline，规格不同）。
/// 失败语义：缺页 OCR 拿不到结果 → 整文档 `Err(NeedsOcr)`，绝不产出缺页
/// 文档（对齐 anydoc「宁报错，不缺页」）。
pub(crate) fn convert_pdf_hybrid(
    path: &Path,
    opts: &ConvertRequest,
    text: DocIR,
    missing_pages: &[u32],
) -> Result<String> {
    let spec = OcrDocSpec {
        path: path.to_path_buf(),
        missing_pages: Some(missing_pages.to_vec()),
        text: Some(text),
    };
    let mut out = convert_pdf_ocr_docs(vec![spec], opts)?;
    match out.pop().map(|(_, r)| r) {
        Some(r) => r,
        None => Err(crate::error::ConvertError::new(
            crate::error::ErrorKind::NeedsOcr,
            crate::error::Stage::Ocr,
            format!("缺页 OCR 未取回结果: {}", fmt_pages(missing_pages)),
        )),
    }
}

/// 混合文档合并（纯函数，可单测）：文字层 DocIR（pass 前）+ 缺页 OCR 结果
/// → 按页号归位 → 跨页表 pass → 渲染。
///
/// 缺页号集合与 `pages` 必须一一对应（调用方已校验），页号重复或某页 OCR
/// 无区块 → `None`（调用方报 NeedsOcr，绝不产出缺页文档）。
/// 页号口径：文字层 `page_no` 取自 `TextItem.page`（1 基），OCR 页同样给 1 基
/// 页号（整篇 OCR 通路用 0 基本地下标，两通路不交叉，golden 不变）。
///
/// `dims`（#6 第 1 步）：与 `pages` **同序**的页尺寸（像素），供 OCR 页写入
/// `PageIR.dims`；长度不足或 `None` 的槽位记 `Unknown`。渲染层不消费它，
/// 故传与不传的输出逐字节相同。
pub(crate) fn merge_hybrid(
    mut text: DocIR,
    pages: &[(u32, oar_ocr::domain::structure::StructureResult)],
    dims: &[Option<(u32, u32)>],
) -> Option<DocIR> {
    let covered: std::collections::BTreeSet<u32> = pages.iter().map(|(p, _)| *p).collect();
    if covered.len() != pages.len() {
        return None;
    }
    // 1) 丢弃缺页位置上的空文字层页（若有），避免与 OCR 页同号并存
    text.pages.retain(|p| !covered.contains(&p.page_no));
    // 2) OCR 页 → 按真实页号入 DocIR（逐页 to_docir 取该页区块，producer
    //    语义与整篇 OCR 一致）
    // #3 契约：to_docir 对单元素切片恒产出**恰好一页**（pages[0]，页序即输入
    //    序）——`next()?` 的 None 分支理论上不可达，保留它是防御下游 oar 改版；
    //    空白扫描页产出的页区块可以为**空**（渲染成功但 OCR 无文本，
    //    hybrid_pdf_with_blank_page_still_succeeds 锁死该行为：空页照常并入，
    //    绝不因"无区块"把整篇文档降级 NeedsOcr）。
    for (i, (page_no, res)) in pages.iter().enumerate() {
        let doc = gfm_adapter::to_docir(
            std::slice::from_ref(res),
            &[dims.get(i).copied().unwrap_or(None)],
        );
        let page = doc.pages.into_iter().next()?;
        text.pages.push(crate::docir::PageIR {
            page_no: *page_no,
            regions: page.regions,
            source: crate::docir::PageSource::Ocr,
            dims: page.dims,
        });
    }
    // 3) 页号升序：跨页表 pass 按 vec 序判定相邻性、渲染按 page_no 分桶归位，
    //    两者都要求页序正确。
    text.pages.sort_by_key(|p| p.page_no);
    // #11：返回 **pass 前** 的 DocIR，终渲染（pass + 按格式投影）由调用方做——
    // 原来这里直接 `finalize_text_docir`，投影层就看不到 IR 了。
    Some(text)
}

/// 跨文档 OCR pipeline 的**单文档规格**（ADR-0005 候选 2 + anydoc 0.2.4 缺页路由）。
pub(crate) struct OcrDocSpec {
    pub path: PathBuf,
    /// `None` = 图片型整篇 OCR（全页渲染）；`Some(1 基缺页号升序)` = 混合文档，
    /// 仅渲染并识别这些页，其余页用 `text`。
    pub missing_pages: Option<Vec<u32>>,
    /// 混合文档的文字层 DocIR（跨页表 pass 前）；整篇 OCR 为 `None`。
    pub text: Option<DocIR>,
}

impl OcrDocSpec {
    /// 图片型文档（整篇 OCR）。
    pub fn scan(path: PathBuf) -> Self {
        Self { path, missing_pages: None, text: None }
    }
}

/// 页号列表 → 人读串（错误 detail 用）。
fn fmt_pages(pages: &[u32]) -> String {
    pages.iter().map(u32::to_string).collect::<Vec<_>>().join(",")
}

/// 跨文档图片型/混合型 PDF OCR（ADR-0005 候选 2 + anydoc 0.2.4 缺页路由）。
///
/// 入参 `specs` 为预分流后的 PDF 列表：`missing_pages = None` 的图片型文档
/// （整篇 OCR，行为与旧实现逐字节一致）；`Some(pages)` 的混合文档（只渲/识别
/// 缺页，其余页用随规格带回的文字层 DocIR）。返回 `Vec<(doc_idx, Result<String>)>`
/// 按 doc_idx 升序——doc_idx 与入参索引一一对应，调用方据此回填每文档 Result。
/// 每文档独立 Result：整文档打开失败/全页渲染失败 → 该 doc_idx 对应 Err（带真实
/// detail，ADR 候选 3），其它文档不受影响（错误隔离）。
///
/// 跨文档 render 闭包逐 doc open + 逐页渲染（每 doc 各自的页集），OCR 池跨文档
/// 消费，文档边界不停顿。pipeline 返回 `(成功页, 渲染错误)` 按复合键升序，此处
/// 按 doc_idx 分组（同组内 page_idx 升序）：图片型 → `gfm_adapter::to_markdown`
/// （产 DocIR → 跨页表 pass → 统一渲染，P1.5，与旧通路字节一致）；混合型 →
/// [`merge_hybrid`]（文字层页 + OCR 页按页号归位后统一 pass + 渲染）。
pub(crate) fn convert_pdf_ocr_docs(
    specs: Vec<OcrDocSpec>,
    opts: &ConvertRequest,
) -> Result<Vec<(usize, Result<String>)>> {
    if specs.is_empty() {
        return Ok(Vec::new());
    }
    // F2：pipeline 主路径直接 `OcrEngine::build`，不经 `ocr_images`，须在此先提交
    // 进程级 ORT 线程池（Ticket A）：任何 ONNX session 创建前调用才生效。
    crate::ocr_engine::init_runtime(&opts.parallel);
    let paths: Vec<PathBuf> = specs.iter().map(|s| s.path.clone()).collect();
    // ADR-0007：质量路由（后验置信度门控）。Auto 时 tiny 渲染+OCR 首文档首页，
    // 平均置信度低于阈值 → 升级 small 全篇重跑；Off 用显式 opts.ocr.tier。
    // 探针失败不阻断，回退显式参数。dpi 始终由用户显式控制（后验只升级 tier，
    // 不再改 dpi）。
    //
    // mineru-basic 是 CLI 与 `ConvertRequest::default()` 的默认档，而本路由的两端
    // （tiny / small）都不是它：让路由在 MinerU 档生效等于把默认流程换掉。故
    // `is_mineru()` 时整段路由跳过（用显式档），见 `quality::routing_applies`。
    let tier = if crate::quality::routing_applies(opts) {
        probe_first_doc_confidence(&paths[0], opts)?
            .map(|needs| {
                if needs {
                    crate::models::OcrTier::Small
                } else {
                    crate::models::OcrTier::Tiny
                }
            })
            .unwrap_or(opts.ocr.tier)
    } else {
        opts.ocr.tier
    };
    let dpi = opts.render.dpi;
    let engine = crate::ocr_engine::OcrEngine::build(tier, opts.ocr.layout)?;
    let timings = std::sync::Arc::new(crate::timing::PageTimings::new());
    // 每 doc 的渲染页集：None = 全页（图片型）；Some = 仅缺页（0 基 pdfium 页号）
    let page_sets: Vec<Option<std::collections::BTreeSet<usize>>> = specs
        .iter()
        .map(|s| {
            s.missing_pages.as_ref().map(|mp| {
                mp.iter().map(|&p| (p as usize) - 1).collect::<std::collections::BTreeSet<usize>>()
            })
        })
        .collect();
    let render_fn = render::render_cross_doc_pages_fn(paths.clone(), dpi, page_sets);
    let (mut results, render_errors, mut page_dims) = crate::pipeline::PagePipeline::new(
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
    timings.report();

    // T2：按页失败重试（仅质量路由可用时，见 `quality::routing_applies`）。首页
    // 门控只决定基础档；pipeline 跑完后逐页 `page_needs_retry`（均值<阈值/无
    // regions/全缺 → 低质量页），用更高档**局部重跑**失败页（重渲染该页 + 更高档
    // OCR），成功页保留。
    // - 只升一档（OcrTier::next），更高档仍失败则保留原结果，防循环；
    // - 重试页渲染失败（pipeline 缺失）→ 保留原结果；
    // - 路由不适用（Off，或基础档为 MinerU）时行为完全不变（golden 稳定）。
    if crate::quality::routing_applies(opts) {
        if let Some(higher) = tier.next() {
            let bad: Vec<(usize, usize)> = results
                .iter()
                .filter(|(_, r)| crate::quality::page_needs_retry(r))
                .map(|(idx, _)| *idx)
                .collect();
            if !bad.is_empty() {
                let higher_engine = crate::ocr_engine::OcrEngine::build(higher, opts.ocr.layout)?;
                let retry_render_fn = render::render_cross_doc_subset_fn(paths.clone(), dpi, bad);
                let (retry_results, _retry_errors, retry_dims) =
                    crate::pipeline::PagePipeline::new(
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
                let retry_map: std::collections::HashMap<(usize, usize), _> =
                    retry_results.into_iter().collect();
                for (idx, res) in results.iter_mut() {
                    if let Some(new) = retry_map.get(idx) {
                        *res = new.clone();
                    }
                }
                page_dims.extend(retry_dims);
            }
        }
    }

    // 按复合键 (doc_idx, page_idx) 升序结果分组——pipeline 已保证页序，
    // 同 doc_idx 组内 page_idx 升序，直接 collect 进 Vec 保序。
    let mut by_doc: BTreeMap<usize, Vec<(usize, oar_ocr::domain::structure::StructureResult)>> =
        BTreeMap::new();
    for ((doc_idx, page_idx), res) in results {
        by_doc.entry(doc_idx).or_default().push((page_idx, res));
    }
    // 整文档失败（哨兵页 usize::MAX 标记打开失败）→ 该 doc_idx 标 Err（带真实 detail）。
    // 单页失败（page < usize::MAX）不标错——该 doc 其余页仍产出，保错误隔离。
    // MinerU 回归用（ANYDOC_DUMP_DIR）：逐页 StructureResult + 页像素尺寸 → JSON，
    // 供框级 IoU / 阅读顺序对比。
    let dump_dir = std::env::var("ANYDOC_DUMP_DIR").ok().filter(|s| !s.is_empty());
    let mut doc_errors = classify_doc_errors(render_errors);
    let mut out = Vec::with_capacity(specs.len());
    for (doc_idx, spec) in specs.into_iter().enumerate() {
        let pages = by_doc.remove(&doc_idx).unwrap_or_default();
        let e = doc_errors.remove(&doc_idx);
        let md = assemble_doc_result(
            spec.text,
            spec.missing_pages.as_deref(),
            e,
            pages,
            doc_idx,
            &dump_dir,
            &page_dims,
            opts.format,
        );
        out.push((doc_idx, md));
    }
    // 整批路径中无任何成功页的 doc（打开失败）——已由 doc_errors 覆盖；若
    // 仍有 doc_idx 完全缺失但无错误（理论不出现），补兜底 Err。
    for (doc_idx, e) in doc_errors {
        out.push((doc_idx, Err(e)));
    }
    out.sort_by_key(|(i, _)| *i);
    Ok(out)
}

/// pipeline 的 `(doc_idx, page_idx) → 位图宽高` 表摊平成与 `pages` **同序**的
/// dims 向量（#6 第 1 步）。
///
/// 单独抽出来是因为这是 dims 通路上**唯一可能悄悄错配**的地方：`to_docir` 按
/// 下标取 dims，而下标来自 `pages` 的迭代序；markdown 里看不见 dims，golden
/// 永远抓不到"页 A 配了页 B 的尺寸"，只能靠 `align_page_dims_*` 两条单测钉住。
/// 查不到 → `None`（该页记 `Unknown`），绝不顺延邻居的尺寸。
fn align_page_dims(
    doc_idx: usize,
    page_indices: &[usize],
    page_dims: &BTreeMap<(usize, usize), (u32, u32)>,
) -> Vec<Option<(u32, u32)>> {
    page_indices.iter().map(|pi| page_dims.get(&(doc_idx, *pi)).copied()).collect()
}

/// 单文档装配：整文档错误优先 → 混合合并（缺页未取回 → NeedsOcr）→ 图片型
/// 整篇 OCR（与旧通路字节一致）。dump 在装配前落盘（覆盖两种路由）。
///
/// 混合校验：pipeline 实际取回的 OCR 页集合必须**恰好等于** `missing`（缺页），
/// 少页（渲染/识别失败）与多页（页号漂移）都判 `Err(NeedsOcr)`——绝不产出
/// 页不完整的文档（anydoc「宁报错，不缺页」）。
#[allow(clippy::type_complexity)]
fn assemble_doc_result(
    text: Option<DocIR>,
    missing: Option<&[u32]>,
    doc_error: Option<crate::error::ConvertError>,
    pages: Vec<(usize, oar_ocr::domain::structure::StructureResult)>,
    doc_idx: usize,
    dump_dir: &Option<String>,
    page_dims: &BTreeMap<(usize, usize), (u32, u32)>,
    fmt: crate::docir::OutputFormat,
) -> Result<String> {
    if let Some(e) = doc_error {
        return Err(e);
    }
    if let Some(dir) = dump_dir {
        let _ = std::fs::create_dir_all(dir);
        let mut manifest = Vec::new();
        for (pi, res) in &pages {
            if let Ok(s) = serde_json::to_string(res) {
                let _ = std::fs::write(format!("{dir}/doc{doc_idx}_page{pi:03}.json"), s);
            }
            let (w, h) = page_dims.get(&(doc_idx, *pi)).copied().unwrap_or((0, 0));
            manifest.push(format!("{pi} {w} {h}"));
        }
        let _ = std::fs::write(format!("{dir}/doc{doc_idx}_dims.txt"), manifest.join("\n") + "\n");
    }
    match text {
        // 混合：OCR 页号 = page_idx + 1（pipeline 的 page_idx 是 0 基 pdfium 页号）
        Some(text) => {
            // dims 与 pages 同序（#6 第 1 步）：按 (doc_idx, page_idx) 查真实位图尺寸，
            // 查不到给 None → 该页 `PageDimsKind::Unknown`（不拿内容外扩冒充）。
            let idxs: Vec<usize> = pages.iter().map(|(pi, _)| *pi).collect();
            let ocr_dims = align_page_dims(doc_idx, &idxs, page_dims);
            let ocr: Vec<(u32, _)> =
                pages.into_iter().map(|(pi, res)| ((pi as u32) + 1, res)).collect();
            let got: std::collections::BTreeSet<u32> =
                ocr.iter().map(|(p, _)| *p).collect::<std::collections::BTreeSet<_>>();
            let want: std::collections::BTreeSet<u32> =
                missing.unwrap_or_default().iter().copied().collect();
            if got != want {
                let mut miss: Vec<u32> = want.difference(&got).copied().collect();
                miss.sort_unstable();
                return Err(crate::error::ConvertError::new(
                    crate::error::ErrorKind::NeedsOcr,
                    crate::error::Stage::Ocr,
                    if miss.is_empty() {
                        format!("doc {doc_idx} 缺页 OCR 结果页号异常（期望 {want:?}）")
                    } else {
                        format!(
                            "doc {doc_idx} 缺页 OCR 未取回结果: {}",
                            fmt_pages(&miss)
                        )
                    },
                ));
            }
            let doc = merge_hybrid(text, &ocr, &ocr_dims).ok_or_else(|| {
                crate::error::ConvertError::new(
                    crate::error::ErrorKind::NeedsOcr,
                    crate::error::Stage::Ocr,
                    format!(
                        "doc {doc_idx} 缺页 OCR 合并失败: {}",
                        fmt_pages(missing.unwrap_or_default())
                    ),
                )
            })?;
            // 混合文档含 OCR 页，但渲染风格按**文字层**口径（与旧
            // `finalize_text_docir` 一致，golden 守护）。
            Ok(crate::docir::finalize(doc, fmt, false))
        }
        // 图片型整篇 OCR（旧行为）
        None => {
            // dims 与 pages **同序**（#6 第 1 步）：to_docir 按下标取 dims，而这里
            // 的页号就是 vec 下标（0 基），故先按 page_idx 升序再摊平尺寸。
            let mut ordered: Vec<_> = pages.into_iter().collect();
            ordered.sort_by_key(|(pi, _)| *pi);
            let idxs: Vec<usize> = ordered.iter().map(|(pi, _)| *pi).collect();
            let dims = align_page_dims(doc_idx, &idxs, page_dims);
            let res: Vec<_> = ordered.into_iter().map(|(_, r)| r).collect();
            // #11：IR 是真相 —— 按格式投影（markdown 与旧 `to_markdown` 字节一致：
            // 同为 pass + render_with_furniture）。
            Ok(crate::docir::finalize(
                gfm_adapter::to_docir(&res, &dims),
                fmt,
                true,
            ))
        }
    }
}

/// 从渲染错误列表提取「整文档错误」：哨兵页 `usize::MAX` 标记打开失败，其余单页
/// 失败（page < usize::MAX）不标错——该 doc 其余页仍产出，保错误隔离（ADR 候选 3）。
fn classify_doc_errors(
    render_errors: Vec<((usize, usize), crate::error::ConvertError)>,
) -> BTreeMap<usize, crate::error::ConvertError> {
    let mut doc_errors: BTreeMap<usize, crate::error::ConvertError> = BTreeMap::new();
    for ((doc_idx, page_idx), e) in render_errors {
        if page_idx == usize::MAX {
            doc_errors.insert(doc_idx, e);
        }
    }
    doc_errors
}

/// ADR-0007（后验）：tiny 渲染+OCR 首文档首页 → 平均置信度 → 是否升级 small。
/// 返回 `Some(true)` 升级、`Some(false)` 维持 tiny；探针失败（渲染/OCR）返回 Ok(None)，
/// 调用方回退显式参数。探针仅 1 页 tiny，开销最小。
///
/// F1：任何一步失败（坏 PDF/缺页/模型缺失）都吞掉返回 Ok(None)，不得使整批 OCR 挂掉。
fn probe_first_doc_confidence(path: &Path, opts: &ConvertRequest) -> Result<Option<bool>> {
    let imgs = match render::render_pdf_pages(path, opts.render.dpi, &[0]) {
        Ok(imgs) => imgs,
        Err(_) => return Ok(None),
    };
    // 探针固定用 tiny：本就是要判定 tiny 是否够用
    let engine =
        match crate::ocr_engine::OcrEngine::build(crate::models::OcrTier::Tiny, opts.ocr.layout) {
            Ok(e) => e,
            Err(_) => return Ok(None),
        };
    let pages = match engine.predict(imgs, 1, None) {
        Ok(p) => p,
        Err(_) => return Ok(None),
    };
    let Some(page) = pages.first() else {
        return Ok(None);
    };
    Ok(Some(crate::quality::needs_upgrade(page)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docir::PageIR;
    use crate::error::runtime;
    use crate::region::{Region, RegionKind};
    use crate::table_grid::{TableCell, TableGrid};
    use oar_ocr::domain::TextRegion;
    use oar_ocr::domain::structure::StructureResult;
    use oar_ocr::processors::BoundingBox;

    fn tr(x0: f32, y0: f32, x1: f32, y1: f32, text: &str) -> TextRegion {
        TextRegion {
            bounding_box: BoundingBox::from_coords(x0, y0, x1, y1),
            text: Some(text.into()),
            ..TextRegion::new(BoundingBox::from_coords(x0, y0, x1, y1))
        }
    }

    /// 单文本行 OCR 页（无 layout 块 → to_docir 降级 order_text_regions）。
    fn ocr_page(text: &str) -> StructureResult {
        StructureResult {
            layout_elements: vec![],
            text_regions: Some(vec![tr(50.0, 100.0, 500.0, 120.0, text)]),
            tables: Vec::new(),
            ..StructureResult::new("t", 0)
        }
    }

    fn body_doc(pages: &[(u32, &str)]) -> DocIR {
        let mut doc = DocIR::default();
        for (no, text) in pages {
            doc.pages.push(PageIR {
                page_no: *no,
                regions: vec![Region::new(0.0, 100.0, 0.0, 10.0, *text)],
                source: crate::docir::PageSource::TextLayerPdf,
                dims: crate::docir::PageDims::default(),
            });
        }
        doc
    }

    fn cell(t: &str) -> TableCell {
        TableCell {
            text: t.into(),
            x: 0.0,
            y: 0.0,
            h: 10.0,
        }
    }

    fn grid_page(page_no: u32, cols: usize, texts: &[&str]) -> PageIR {
        PageIR {
            page_no,
            regions: vec![Region::new(0.0, 0.0, 0.0, 0.0, String::new())
                .with_kind(RegionKind::Grid(TableGrid {
                    cols,
                    header: vec![],
                    rows: texts.chunks(cols).map(|c| c.iter().map(|s| cell(s)).collect()).collect(),
                    has_header: false,
                }))],
            source: crate::docir::PageSource::TextLayerPdf,
            dims: crate::docir::PageDims::default(),
        }
    }

    /// #11：`merge_hybrid` 返回 **pass 前** 的 DocIR（投影层要看到 IR），
    /// 老断言是按markdown 写的，这里补一步终渲染——口径与旧
    /// `finalize_text_docir`（跨页表 pass + render）逐字节一致。
    ///
    /// 用 `finalize_with_opts(.., RenderOpts::OFF)`：本helper 下的断言验的是
    /// **页序 / 空页丢弃 / 跨页表定格**，与 #9 gap-A/B 的开关无关，固定在
    /// OFF 档才不会让失败信息指向错误的行。
    fn merge_md(
        text: DocIR,
        pages: &[(u32, oar_ocr::domain::structure::StructureResult)],
    ) -> String {
        let doc = merge_hybrid(text, pages, &[]).expect("merge ok");
        crate::docir::finalize_with_opts(
            doc,
            crate::docir::OutputFormat::Markdown,
            false,
            crate::docir::render::RenderOpts::OFF,
        )
    }

    /// 缺页 OCR 结果按真实页号插回：1(文字) + 2(OCR) + 3(文字) 顺序输出。
    #[test]
    fn merge_hybrid_inserts_ocr_pages_in_page_order() {
        let text = body_doc(&[(1, "第一页"), (3, "第三页")]);
        let pages = vec![(2u32, ocr_page("第二页扫描件"))];
        let md = merge_md(text, &pages);
        let i1 = md.find("第一页").expect("p1");
        let i2 = md.find("第二页扫描件").expect("p2 ocr");
        let i3 = md.find("第三页").expect("p3");
        assert!(i1 < i2 && i2 < i3, "页序错乱: {md}");
        // 合并页标 Ocr 源渲染（正文行 + 页间空行），不吞文字层页
        assert_eq!(md.matches("页").count(), 3);
    }

    /// 页号重复（上游数据异常）→ None，调用方报 NeedsOcr 而非产缺页文档。
    #[test]
    fn merge_hybrid_rejects_duplicate_page_no() {
        let text = body_doc(&[(1, "第一页")]);
        let pages = vec![(2u32, ocr_page("甲")), (2u32, ocr_page("乙"))];
        assert!(merge_hybrid(text, &pages, &[]).is_none());
    }

    /// OCR 页与文字层同号页共存时，文字层空占位页被 OCR 页替换（不双份）。
    #[test]
    fn merge_hybrid_replaces_same_page_no_placeholder() {
        let text = body_doc(&[(1, "第一页"), (2, "")]);
        let pages = vec![(2u32, ocr_page("第二页扫描件"))];
        let md = merge_md(text, &pages);
        assert!(md.contains("第二页扫描件"));
        // 空文字层页被丢弃后只剩两页段
        assert_eq!(md, "第一页\n\n第二页扫描件");
    }

    /// 文字层跨页表被夹在中间的 OCR 页（无 Grid）定格：p1/p3 不合并，
    /// 各自独立成表（cross_page_table pass 以"无 Grid 页"为 flush 边界）。
    #[test]
    fn merge_hybrid_ocr_page_breaks_cross_page_grid_merge() {
        let mut text = DocIR::default();
        text.pages.push(grid_page(1, 2, &["a1", "a2"]));
        text.pages.push(grid_page(3, 2, &["b1", "b2"]));
        let pages = vec![(2u32, ocr_page("第二页扫描件"))];
        let md = merge_md(text, &pages);
        let n_open = md.matches("<table").count();
        let n_close = md.matches("</table>").count();
        assert_eq!(n_open, 2, "两表应各自定格，got {n_open}: {md}");
        assert_eq!(n_open, n_close);
        assert!(md.contains("第二页扫描件"));
    }

    /// 摊平按**传入页序**逐槽查表（不是按 map 的键序）：页序 2/0/1 必须得到
    /// 2/0/1 各自的尺寸，错一个槽位就是"页 A 配页 B 的分母"。
    #[test]
    fn align_page_dims_follows_input_order() {
        let mut m = BTreeMap::new();
        m.insert((0, 0), (100u32, 200u32));
        m.insert((0, 1), (300, 400));
        m.insert((0, 2), (500, 600));
        let got = align_page_dims(0, &[2, 0, 1], &m);
        assert_eq!(got, vec![Some((500, 600)), Some((100, 200)), Some((300, 400))]);
    }

    /// 跨文档隔离（同一批多文档时 doc_idx 不同）+ 缺槽为 `None`（不得顺延邻居）。
    #[test]
    fn align_page_dims_isolates_docs_and_leaves_gaps_none() {
        let mut m = BTreeMap::new();
        m.insert((0, 0), (100u32, 200u32));
        m.insert((1, 0), (900, 1000));
        assert_eq!(align_page_dims(0, &[0, 1], &m), vec![Some((100, 200)), None]);
        assert_eq!(align_page_dims(1, &[0, 1], &m), vec![Some((900, 1000)), None]);
        assert_eq!(align_page_dims(7, &[0], &m), vec![None], "未知文档整篇 Unknown");
    }

    /// fmt_pages：错误 detail 用的页号串。
    #[test]
    fn fmt_pages_joins_with_comma() {
        assert_eq!(fmt_pages(&[2, 5, 9]), "2,5,9");
        assert_eq!(fmt_pages(&[]), "");
    }

    // ── assemble_doc_result：混合装配的 NeedsOcr 契约 ──

    fn assemble(
        text: Option<DocIR>,
        missing: Option<&[u32]>,
        doc_error: Option<crate::error::ConvertError>,
        pages: Vec<(usize, StructureResult)>,
    ) -> crate::Result<String> {
        assemble_doc_result(
            text,
            missing,
            doc_error,
            pages,
            0,
            &None,
            &std::collections::BTreeMap::new(),
            crate::docir::OutputFormat::Markdown,
        )
    }

    /// 混合装配成功：缺页 {2} OCR 取回 → 合并输出含 OCR 页与文字层页。
    #[test]
    fn assemble_hybrid_ok_when_missing_pages_recovered() {
        let r = assemble(
            Some(body_doc(&[(1, "第一页"), (3, "第三页")])),
            Some(&[2]),
            None,
            vec![(1, ocr_page("第二页扫描件"))],
        );
        let md = r.expect("应成功");
        assert!(md.contains("第一页") && md.contains("第二页扫描件") && md.contains("第三页"));
    }

    /// 缺页未全部取回（页 2 渲染/识别失败）→ Err(needsOcr)，绝不产出缺页文档。
    #[test]
    fn assemble_hybrid_missing_page_yields_needs_ocr_err() {
        let r = assemble(
            Some(body_doc(&[(1, "第一页"), (3, "第三页"), (4, "第四页")])),
            Some(&[2, 3]),
            None,
            vec![(3, ocr_page("第四页扫描件"))],
        );
        let e = r.expect_err("缺页未取回必须 Err");
        assert_eq!(e.code(), "needsOcr", "{e}");
        assert!(e.to_string().contains("2"), "detail 应列出未取回页: {e}");
    }

    /// OCR 结果页号漂移（取回页 ∉ 缺页集）→ 同样 Err(needsOcr)（页不完整宁报错）。
    #[test]
    fn assemble_hybrid_unexpected_page_yields_needs_ocr_err() {
        let r = assemble(
            Some(body_doc(&[(1, "第一页")])),
            Some(&[2]),
            None,
            vec![(5, ocr_page("漂移页"))],
        );
        let e = r.expect_err("页号漂移必须 Err");
        assert_eq!(e.code(), "needsOcr", "{e}");
    }

    /// 整文档错误优先于混合合并（打开失败 → 原 Err 透传，不进 merge）。
    #[test]
    fn assemble_doc_error_takes_precedence() {
        let e = crate::error::ConvertError::new(
            crate::error::ErrorKind::Malformed,
            crate::error::Stage::Render,
            "打不开",
        );
        let r = assemble(Some(body_doc(&[(1, "第一页")])), Some(&[2]), Some(e), vec![]);
        let err = r.expect_err("doc_error 必须优先");
        assert_eq!(err.code(), "malformed");
    }

    /// 图片型（text=None）旧行为不变：OCR 页直接 to_markdown。
    #[test]
    fn assemble_scan_path_unchanged() {
        let r = assemble(None, None, None, vec![(0, ocr_page("整页扫描"))]);
        assert!(r.expect("scan 装配应成功").contains("整页扫描"));
    }

    /// ADR 候选 3 契约：哨兵页 usize::MAX → 该 doc 标 Err；单页失败（page < MAX）
    /// 不标错（保错误隔离）。多 doc 只影响对应 doc。
    #[test]
    fn classify_doc_errors_sentinel_marks_whole_doc() {
        let errs = vec![
            ((0, usize::MAX), runtime(crate::error::Stage::Render, None, "打开 0 失败")),
            ((1, 0), runtime(crate::error::Stage::Render, Some(0), "单页渲染失败")),
            ((2, usize::MAX), runtime(crate::error::Stage::Render, None, "打开 2 失败")),
        ];
        let doc_errors = classify_doc_errors(errs);
        // 哨兵页 doc 标记为 Err
        assert_eq!(doc_errors.len(), 2);
        assert!(doc_errors.contains_key(&0));
        assert!(doc_errors.contains_key(&2));
        // 单页失败(doc 1)不标错
        assert!(!doc_errors.contains_key(&1));
    }

    /// ADR 候选 3 契约：空渲染错误 → 空 doc 错误。
    #[test]
    fn classify_doc_errors_empty_input() {
        assert!(classify_doc_errors(vec![]).is_empty());
    }
}
