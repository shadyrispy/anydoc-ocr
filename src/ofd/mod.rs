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
use std::path::Path;

use image::RgbImage;
use ofd_core::{OfdReader, RenderOptions};

use crate::ConvertRequest;
use crate::docir::{DocIR, PageSource};
use crate::error::{Result as CResult, Stage, from_ofd_error, runtime};
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
enum PageData {
    /// 纯文字层：坐标行 `Region`（`x_min/x_max/y_min/y_max/文本`）。
    Text(Vec<Region>),
    /// F3 坏字体乱码页：第一遍已立即渲染（少量，需保留 fallback 文字层）。
    /// img 用 Option 以便第二遍 `take` 转移所有权，避免双持（T04）。
    OcrFull(Option<RgbImage>),
    /// 图片型页（text_len < 阈值且 img_count > 0）：第一遍**不渲染**，
    /// 记录 (body_idx, page_idx) 待第二遍 P3 流水线渲染+OCR（ADR-0002）。
    /// 全图片型文档的渲染被 OCR 掩盖，峰值内存从 N×页图降到 ~2×页图。
    OcrPendingImage { body_idx: usize, page_idx: usize },
}

/// OFD → Markdown 总入口（P1.8 拆阶段）：分类 → 质量路由 → OCR → DocIR 装配。
pub fn convert_ofd(path: &Path, opts: &ConvertRequest, ofd_force_ocr: bool) -> CResult<String> {
    let mut t = StageTimer::new();
    // F2：OFD 主路径直接 `OcrEngine::build`（探针/路径 B），不经 `ocr_images`，
    // 须在首个 ONNX session 创建前提交进程级 ORT 线程池（Ticket A）。
    crate::ocr_engine::init_runtime(&opts.parallel);
    let mut reader = OfdReader::open(path).map_err(from_ofd_error)?;
    // clone 出来避免遍历时与 reader 的 &mut 借用冲突
    let doc_bodies = reader.ofd().doc_bodies.clone();

    // 第一遍：逐页判定类型（文字层 / F3 乱码页立即渲染 / 图片型页延迟渲染）。
    let mut pages = classify_pages(&mut reader, &doc_bodies, opts, ofd_force_ocr)?;

    // 第二遍：OCR（F3 乱码页批量 + 图片型页 P3 流水线），tier 由质量路由决定。
    let has_ocr_pages = pages
        .iter()
        .any(|p| matches!(p, PageData::OcrFull(_) | PageData::OcrPendingImage { .. }));
    let route_tier = probe_route_tier(&mut reader, &doc_bodies, opts, has_ocr_pages);
    let mut full_out = ocr_garbled_pages(&mut pages, route_tier, opts)?;
    full_out.extend(ocr_pending_pages(&pages, path, route_tier, opts, &mut t)?);
    t.stage("gfm");

    // 第三遍：DocIR 装配（跨页表合并 pass + 统一渲染）。
    Ok(assemble_docir(&pages, &mut full_out))
}

/// 第一遍：逐页判定类型并收集数据。渲染在循环内完成（需要 per-body `doc`）。
/// 注：原先为"末页强制入可疑集"预扫描过一遍全局总页数，该强制已移除（Ticket B），
/// 预扫描随之删除——省掉一轮跨 doc body 的 load_document。
fn classify_pages(
    reader: &mut OfdReader<std::fs::File>,
    doc_bodies: &[ofd_core::model::ofd::DocBody],
    opts: &ConvertRequest,
    ofd_force_ocr: bool,
) -> CResult<Vec<PageData>> {
    let mut pages: Vec<PageData> = Vec::new();
    for (body_idx, body) in doc_bodies.iter().enumerate() {
        let doc = reader.load_document(body).map_err(from_ofd_error)?;
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
                    Ok(img) => pages.push(PageData::OcrFull(Some(img))),
                    Err(_) => pages.push(PageData::Text(to_regions(texts))),
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
                pages.push(PageData::Text(to_regions(texts)));
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
    if !(has_ocr_pages && opts.quality_route == crate::quality::QualityRoute::Auto) {
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
fn ocr_garbled_pages(
    pages: &mut [PageData],
    route_tier: crate::models::OcrTier,
    opts: &ConvertRequest,
) -> CResult<BTreeMap<u32, String>> {
    let mut full_pages: Vec<u32> = Vec::new();
    let mut full_imgs: Vec<RgbImage> = Vec::new();
    for (i, d) in pages.iter_mut().enumerate() {
        if let PageData::OcrFull(img) = d
            && let Some(im) = img.take()
        {
            full_pages.push(i as u32);
            full_imgs.push(im);
        }
    }
    let mut full_out: BTreeMap<u32, String> = BTreeMap::new();
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
    for (page, res) in full_pages.into_iter().zip(results) {
        full_out.insert(page, gfm_adapter::to_markdown(std::slice::from_ref(&res)));
    }
    Ok(full_out)
}

/// 路径 B：图片型页 P3 流水线——render_fn 闭包内重新 open reader + load + 逐页渲染，
/// 与 OCR 池并发。OfdReader 非 Send → 渲染在专属线程（闭包内 open，不跨线程）。
fn ocr_pending_pages(
    pages: &[PageData],
    path: &Path,
    route_tier: crate::models::OcrTier,
    opts: &ConvertRequest,
    t: &mut StageTimer,
) -> CResult<BTreeMap<u32, String>> {
    // (全局页下标 gi, body_idx, page_idx)
    let pending: Vec<(usize, usize, usize)> = pages
        .iter()
        .enumerate()
        .filter_map(|(gi, d)| match d {
            PageData::OcrPendingImage { body_idx, page_idx } => Some((gi, *body_idx, *page_idx)),
            _ => None,
        })
        .collect();
    let mut full_out: BTreeMap<u32, String> = BTreeMap::new();
    if pending.is_empty() {
        return Ok(full_out);
    }
    t.stage("render");
    let engine = crate::ocr_engine::OcrEngine::build(route_tier, opts.ocr.layout)?;
    let timings = std::sync::Arc::new(crate::timing::PageTimings::new());
    let path = path.to_path_buf();
    let dpi = opts.render.dpi;
    let render_fn = move |tx: std::sync::mpsc::SyncSender<crate::pipeline::RenderItem>| -> crate::error::Result<()> {
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
            // 回退整页渲染（混合页/多图块/直提失败）
            match reader.render_page_to_image(&doc, *page_idx, &RenderOptions::with_dpi(dpi.into())) {
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
    };
    let (results, _render_errors) = crate::pipeline::PagePipeline::new(
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
    // pipeline 返回 Vec<((doc_idx, page_idx), res)> 按复合键升序。
    // OFD 单文档 doc_idx 恒 0，page_idx = gi 直接映射 full_out；
    // 渲染失败页 gi 缺失 → full_out 无该页 → 第三遍装配跳过（容错）
    for ((_doc_idx, gi), res) in results {
        full_out.insert(gi as u32, gfm_adapter::to_markdown(std::slice::from_ref(&res)));
    }
    Ok(full_out)
}

/// 第三遍：输出装配（P1.5 DocIR producer）。跨页表格（文字层网格，免 OCR）
/// 合并由 `docir::passes::cross_page_table` 承担；图片型 OCR/普通行各自落页。
fn assemble_docir(pages: &[PageData], full_out: &mut BTreeMap<u32, String>) -> String {
    let mut doc = DocIR::default();
    for (page, data) in pages.iter().enumerate() {
        let page = page as u32;
        match data {
            PageData::OcrFull(_) | PageData::OcrPendingImage { .. } => {
                // 图片型/乱码页：OCR 成品段（gfm_adapter 已产出行 + 表格 HTML 的
                // 最终 markdown），作为 PreRendered 区块原样落页。
                if let Some(md) = full_out.remove(&page) {
                    doc.push_page(
                        page,
                        PageSource::TextLayerOfd,
                        vec![Region::new(0.0, 0.0, 0.0, 0.0, md)
                            .with_kind(RegionKind::PreRendered)],
                    );
                }
            }
            PageData::Text(lines) => {
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
                    doc.push_page(
                        page,
                        PageSource::TextLayerOfd,
                        vec![Region::new(0.0, 0.0, 0.0, 0.0, String::new())
                            .with_kind(RegionKind::Grid(grid))],
                    );
                    continue;
                }
                // 2) 普通页：文字层行（F4 加标题前缀）为 Body 区块，docir 渲染层
                //    按页 join("\n")（历史无标题空行语义）。
                let md = reading_order::postprocess_lines(reading_order::order_text_regions(lines));
                let out: Vec<Region> = crate::text_health::apply_title_prefixes(&md, &[], true)
                    .into_iter()
                    .map(|l| Region::new(0.0, 0.0, 0.0, 0.0, l))
                    .collect();
                doc.push_page(page, PageSource::TextLayerOfd, out);
            }
        }
    }
    crate::docir::passes::cross_page_table::run(&mut doc);
    doc.render()
}
