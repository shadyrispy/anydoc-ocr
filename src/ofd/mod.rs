//! OFD 通道：文字型走 ofd-core 文本提取；图片型走渲染+OCR 管线
//!
//! 页型判定：逐页统计文本量，低于阈值且存在图像对象则视为图片型（或
//! `--ofd-force-ocr` 强制），走与 PDF 共用的 OCR 回退管线；否则按坐标提取
//! TextObject 文本流，保持与 pdf-inspector 风格一致的纯文本 GFM。
//!
//! 与 PDF 文字层对齐的增强：
//! - F1 文字层表格重建 + 跨页合并（`table_grid`），输出段表按首表页落位；
//! - F2 首/末页"可疑表格页"探针：Ticket B 已移除（无证据的启发式召回，代价是
//!   每文档 1~2 次整页渲染+版面 OCR），表格全部由 F1 网格重建承担；
//! - F3 坏字体乱码页检测：U+FFFD/私有区/控制字符占比超标 → 该页改走整页 OCR；
//! - F4 标题前缀：编号启发式命中且行 <=60 字符、未带 `#` → 加 `#` 前缀。
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;

use image::{RgbImage, RgbaImage};
use ofd_core::model::graphics::PageBlock;
use ofd_core::model::page::PageObject;
use ofd_core::{LoadedDocument, OfdReader, RenderOptions};

use crate::emitter::{DocumentEmitter, FlushFormat};
use crate::gfm_adapter;
use crate::ocr_engine;
use crate::reading_order;
use crate::region::Region;
use crate::table_grid;
use crate::timing::StageTimer;
use crate::{ConvertOptions, Result as CResult};

/// OFD 文字层提取的文本行：x0, x1, y0, y1（左、右、上、下，文档坐标），text。
#[derive(Debug, Clone)]
pub(crate) struct OfdTextLine {
    pub x0: f64,
    pub x1: f64,
    pub y0: f64,
    pub y1: f64,
    pub text: String,
}

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

/// OFD → Markdown 总入口。
pub fn convert_ofd(path: &Path, opts: &ConvertOptions) -> CResult<String> {
    let mut t = StageTimer::new();
    let mut reader = OfdReader::open(path).map_err(|e| anyhow::anyhow!("打开 OFD 失败: {e}"))?;
    // clone 出来避免遍历时与 reader 的 &mut 借用冲突
    let doc_bodies = reader.ofd().doc_bodies.clone();

    // 第一遍：逐页判定类型并收集数据。渲染在循环内完成（需要 per-body `doc`）。
    // 注：原先为"末页强制入可疑集"预扫描过一遍全局总页数，该强制已移除（见下），
    // 预扫描随之删除——省掉一轮跨 doc body 的 load_document。
    let mut pages: Vec<PageData> = Vec::new();

    for (body_idx, body) in doc_bodies.iter().enumerate() {
        let doc = reader
            .load_document(body)
            .map_err(|e| anyhow::anyhow!("装载 OFD 文档失败: {e}"))?;
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
            let is_image =
                opts.ofd_force_ocr || (text_len < IMAGE_PAGE_MIN_TEXT_CHARS && img_count > 0);

            if is_image {
                // P3：图片型页延迟渲染——记录 (body_idx, page_idx) 待第二遍流水线，
                // 不在第一遍立即渲染（避免全图片型文档的 N 次串行渲染不被 OCR 掩盖）。
                pages.push(PageData::OcrPendingImage {
                    body_idx,
                    page_idx: idx,
                });
            } else if is_garbled_text(&texts) {
                // F3：坏字体乱码页 → 整页 OCR（渲染失败时回落文字层，不炸文档）
                match render_page(&mut reader, &doc, idx, opts) {
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
                let lines = to_regions(texts);
                // Ticket B：移除首/末页"强制入可疑表格页探针集"。OFD 文字层每个
                // TextObject 就是完整一行、拿不到行内 x 分离段，无法像 PDF 那样用
                // 证据判定表格，首/末页强制本质是纯启发式的无证据召回，代价却是每
                // 文档 1~2 次整页渲染 + 版面 OCR。表格改由 F1 网格重建（免 OCR）承
                // 担，与 PDF 侧取舍对称：PDF 同样已删首/末页强制，只是它另有
                // `probe_last_page_table` 兜底末页，OFD 无对应探针。
                pages.push(PageData::Text(lines));
            }
        }
    }

    // 第二遍：OCR。两路——F3 乱码页（已渲染 img）批量 OCR + 图片型页（待渲染）P3 流水线。
    // F3 通常少量；图片型页走 render↔OCR 流水线（ADR-0002），渲染被 OCR 掩盖。
    let mut full_pages: Vec<u32> = Vec::new();
    let mut full_imgs: Vec<RgbImage> = Vec::new();
    // T04：第二遍直接 `take` 转移 img 所有权（不 clone），峰值从 2× 降到 1×。
    for (i, d) in pages.iter_mut().enumerate() {
        if let PageData::OcrFull(img) = d
            && let Some(im) = img.take()
        {
            full_pages.push(i as u32);
            full_imgs.push(im);
        }
    }
    let mut full_out: BTreeMap<u32, String> = BTreeMap::new();

    // 路径 A：F3 乱码页批量 OCR（少量，已渲染）
    if !full_imgs.is_empty() {
        let timings = crate::timing::PageTimings::new();
        let results = ocr_engine::ocr_images(
            full_imgs,
            opts.ocr_tier,
            opts.ocr_layout,
            opts.threads,
            if timings.enabled() { Some(&timings) } else { None },
        )?;
        timings.report();
        for (page, res) in full_pages.into_iter().zip(results) {
            full_out.insert(
                page,
                gfm_adapter::structure_results_to_gfm(std::slice::from_ref(&res)),
            );
        }
    }

    // 路径 B：图片型页 P3 流水线——render_fn 闭包内重新 open reader + load + 逐页渲染，
    // 与 rayon OCR 池并发。OfdReader 非 Send → 渲染在专属线程（闭包内 open，不跨线程）。
    let pending: Vec<(usize, usize, usize)> = pages
        .iter()
        .enumerate()
        .filter_map(|(gi, d)| match d {
            PageData::OcrPendingImage { body_idx, page_idx } => Some((gi, *body_idx, *page_idx)),
            _ => None,
        })
        .collect();
    if !pending.is_empty() {
        t.stage("render");
        let engine = crate::ocr_engine::OcrEngine::build(opts.ocr_tier, opts.ocr_layout)?;
        let timings = std::sync::Arc::new(crate::timing::PageTimings::new());
        let path = path.to_path_buf();
        let dpi = opts.dpi;
        let render_fn = move |tx: std::sync::mpsc::SyncSender<crate::pipeline::RenderItem>| -> anyhow::Result<()> {
            let mut reader = OfdReader::open(&path)
                .map_err(|e| anyhow::anyhow!("流水线内重新打开 OFD 失败: {e}"))?;
            let bodies = reader.ofd().doc_bodies.clone();
            for (gi, body_idx, page_idx) in &pending {
                let body = &bodies[*body_idx];
                let doc = reader
                    .load_document(body)
                    .map_err(|e| anyhow::anyhow!("流水线内装载文档失败: {e}"))?;
                // 内联 render_page 逻辑（opts 非 'static，闭包用捕获的 dpi）。
                // 单文档调用方：doc_idx 固定 0，page_idx = gi（OFD 扁平页序）。
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
                            anyhow::anyhow!("渲染 OFD 页 {gi} 失败: {e}"),
                        )));
                    }
                }
            }
            Ok(())
        };
        let results = crate::pipeline::PagePipeline::new(
            render_fn,
            engine,
            opts.threads,
            if timings.enabled() { Some(timings.clone()) } else { None },
        )
        .run()?;
        t.stage("ocr");
        timings.report();
        // pipeline 返回 Vec<((doc_idx, page_idx), res)> 按复合键升序。
        // OFD 单文档 doc_idx 恒 0，page_idx = gi 直接映射 full_out；
        // 渲染失败页 gi 缺失 → full_out 无该页 → 第三遍装配跳过（容错）
        for ((_doc_idx, gi), res) in results {
            let page = gi as u32;
            full_out.insert(
                page,
                gfm_adapter::structure_results_to_gfm(std::slice::from_ref(&res)),
            );
        }
    }
    t.stage("gfm");

    // 第三遍：输出装配。段表 BTreeMap<u32,String> 按页号保序；跨页表格（文字层网格，
    // 免 OCR）挂起、在首表页 flush；图片型 OCR/普通行各自落段。
    let mut emitter = DocumentEmitter::new(FlushFormat::Text);
    for (page, data) in pages.iter().enumerate() {
        let page = page as u32;
        match data {
            PageData::OcrFull(_) | PageData::OcrPendingImage { .. } => {
                // 图片型/乱码页：先冲掉挂起的跨页表，再落 OCR 段。
                emitter.flush_pending();
                if let Some(md) = full_out.remove(&page) {
                    emitter.push_segment(page, &md);
                    emitter.push_segment(page, "\n\n");
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
                    emitter.emit_grid(grid, page);
                    continue;
                }
                // 2) 普通页：冲掉挂起跨页表，输出文字层行（F4 加标题前缀）。
                emitter.flush_pending();
                let md = reading_order::postprocess_lines(reading_order::order_text_regions(lines));
                let md = crate::text_health::apply_title_prefixes(&md, &[], true).join("\n");
                emitter.push_segment(page, &md);
                emitter.push_segment(page, "\n\n");
            }
        }
    }
    emitter.flush_pending();
    Ok(emitter.finish())
}

/// 渲染一页为 RGB 图（图片型分支同参；`dpi` 控制分辨率）。
fn render_page(
    reader: &mut OfdReader<File>,
    doc: &LoadedDocument,
    idx: usize,
    opts: &ConvertOptions,
) -> CResult<RgbImage> {
    let img: RgbaImage = reader
        .render_page_to_image(doc, idx, &RenderOptions::with_dpi(opts.dpi.into()))
        .map_err(|e| anyhow::anyhow!("渲染 OFD 第 {idx} 页失败: {e}"))?;
    Ok(image::DynamicImage::ImageRgba8(img).to_rgb8())
}

/// `OfdTextLine` 文本行 → `Region`（`f32` 区域，reading_order / table_grid 共用）。
fn to_regions(texts: Vec<OfdTextLine>) -> Vec<Region> {
    texts
        .into_iter()
        .map(|line| {
            Region::new(
                line.x0 as f32,
                line.x1 as f32,
                line.y0 as f32,
                line.y1 as f32,
                line.text,
            )
        })
        .collect()
}

/// F3 坏字体乱码检测：统计 U+FFFD 替换符 / 私有区（U+E000..U+F8FF）/ 控制字符。
/// 整页字符数须超过 [`crate::text_health::GARBLED_MIN_TOTAL_CHARS`] 且坏字符占比 >=
/// [`crate::text_health::GARBLED_BAD_PERCENT_THRESHOLD`]%（`bad*100 >= total*20`）才判乱码，
/// 避免少量误报（如目录点线符的私有区字符）触发整页 OCR。字符分类与阈值常量均收敛于
/// `text_health`（PDF/OFD 共用）。
fn is_garbled_text(texts: &[OfdTextLine]) -> bool {
    let chars = texts.iter().flat_map(|line| line.text.chars());
    crate::text_health::has_garbled_chars(
        chars,
        crate::text_health::GARBLED_MIN_TOTAL_CHARS,
        crate::text_health::GARBLED_BAD_PERCENT_THRESHOLD,
    )
}

/// 收集一页所有 TextObject 的文本，返回 `OfdTextLine`（`x0/x1/y0/y1/行文本`），
/// 坐标为**页面坐标**（原点左上、y 向下，与 `reading_order` "小=上" 约定一致）。
///
/// 区域优先取对象真实页面包围盒（`boundary`）：x_min=boundary.x，
/// x_max=boundary.x+width，y_min=boundary.y，y_max=boundary.y+height——行宽真实
/// 才能让跨整页的页眉/页脚命中 `reading_order::is_full` 的整宽判定。若某对象
/// boundary 宽/高退化（0/负数/NaN，应大于 0），退回单点区域
/// `(x, x, y, y+1.0)`（x/y 为首字符经 boundary 平移 + CTM 变换后的页面坐标），
/// 保证不 panic 也不产生退化区域。
///
/// `TextCode` 的 X/Y 是对象局部坐标（同一对象内相对原点），实际页面位置需经
/// 对象边界平移 + CTM 变换得出：`page = boundary + CTM(code)`。OFD 页面坐标系
/// 原点在左上、y 轴向下（`render` 的 `page_to_device` 直接把物理区左上角映射到
/// 设备原点），故返回的 y 已是"越小越靠上"，与 `reading_order` 约定一致。
fn collect_text_lines(page: &PageObject) -> Vec<OfdTextLine> {
    let mut out = Vec::new();
    if let Some(content) = &page.content {
        for layer in &content.layers {
            collect_text_blocks(&layer.objects, &mut out);
        }
    }
    out
}

/// 斜向旋转水印/装饰文字过滤：从 CTM 线性部分计算文本旋转角
/// （`atan2(b, a)`，单位度，归一化到 [0,360)），若偏离 {0,90,180,270}
/// 超过 ±12° 则视为斜排水印/装饰文字 → 返回 `true`（跳过）。
///
/// 保留 0°/180°（横排正文）与 90°/270°（竖排正文，如中文公文竖排）——
/// 竖排是合法正文，不得误删。CTM 缺省或畸形（非 6 元）一律视为轴对齐，
/// 返回 `false`（保留），与调用侧默认一致。角度 NaN 时比较均为 false → 保留。
///
/// 实现：`deg % 90.0` 到最近轴角度的角距，360↔0 环绕由取模自动处理；
/// 1e-9 容差吸收 cos/sin↔atan2 往返的浮点误差（如 348° 重构为
/// 347.99999999999994 的边界抖动），实际角度分辨率不受影响。
fn ctm_is_watermark_angle(ctm: &[f64]) -> bool {
    if ctm.len() != 6 {
        return false;
    }
    let deg = ctm[1].atan2(ctm[0]).to_degrees();
    let deg = (deg % 360.0 + 360.0) % 360.0; // 归一化到 [0,360)
    const TOL: f64 = 12.0;
    let r = deg % 90.0;
    let nearest_axis_dist = r.min(90.0 - r);
    nearest_axis_dist > TOL + 1e-9
}

fn collect_text_blocks(blocks: &[PageBlock], out: &mut Vec<OfdTextLine>) {
    for b in blocks {
        match b {
            PageBlock::Text(t) => {
                // 斜向旋转的 TextObject（如"太原市人民政府公报"对角水印）在抽取前直接跳过；
                // 竖排（90/270°）与横排（0/180°）正文不受影响。
                if let Some(m) = t.ctm.as_ref()
                    && ctm_is_watermark_angle(m.as_slice())
                {
                    continue;
                }
                let mut codes: Vec<(f64, &str)> = t
                    .text_codes
                    .iter()
                    .filter_map(|c| c.text.as_deref().map(|txt| (c.x.unwrap_or(0.0), txt)))
                    .collect();
                codes.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));
                let line: String = codes.iter().map(|(_, t)| *t).collect();
                if line.trim().is_empty() {
                    continue;
                }
                // TextCode 首字符局部坐标 → 页面坐标：boundary 平移 + CTM 变换。
                let (lx, ly) = t
                    .text_codes
                    .first()
                    .map(|c| (c.x.unwrap_or(0.0), c.y.unwrap_or(0.0)))
                    .unwrap_or((0.0, 0.0));
                let (a, b_, c, d, e, f) = match t.ctm.as_ref().map(|m| m.as_slice()) {
                    Some(m) if m.len() == 6 => (m[0], m[1], m[2], m[3], m[4], m[5]),
                    _ => (1.0, 0.0, 0.0, 1.0, 0.0, 0.0),
                };
                let x = t.boundary.x + a * lx + c * ly + e;
                let y = t.boundary.y + b_ * lx + d * ly + f;
                if t.boundary.width > 0.0 && t.boundary.height > 0.0 {
                    // 真实页面包围盒：让跨整页的页眉/页脚获得整行宽度。
                    let x0 = t.boundary.x;
                    let x1 = t.boundary.x + t.boundary.width;
                    let y0 = t.boundary.y;
                    let y1 = t.boundary.y + t.boundary.height;
                    out.push(OfdTextLine {
                        x0,
                        x1,
                        y0,
                        y1,
                        text: line,
                    });
                } else {
                    // boundary 退化：退回旧单点行为（首字符坐标），不 panic。
                    out.push(OfdTextLine {
                        x0: x,
                        x1: x,
                        y0: y,
                        y1: y + 1.0,
                        text: line,
                    });
                }
            }
            PageBlock::Block(g) => collect_text_blocks(&g.objects, out),
            _ => {}
        }
    }
}

/// 统计一页内 ImageObject 数量（用于页型判定）。
fn count_images(page: &PageObject) -> usize {
    let mut n = 0;
    if let Some(content) = &page.content {
        for layer in &content.layers {
            count_image_blocks(&layer.objects, &mut n);
        }
    }
    n
}

fn count_image_blocks(blocks: &[PageBlock], n: &mut usize) {
    for b in blocks {
        match b {
            PageBlock::Image(_) => *n += 1,
            PageBlock::Block(g) => count_image_blocks(&g.objects, n),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 角度 → CTM 线性矩阵 [cos,sin,-sin,cos,0,0]。
    fn ctm(deg: f64) -> [f64; 6] {
        let r = deg.to_radians();
        [r.cos(), r.sin(), -r.sin(), r.cos(), 0.0, 0.0]
    }

    #[test]
    fn ctm_watermark_angle_detection() {
        // 轴对齐横排：0°（单位矩阵）→ 保留
        assert!(!ctm_is_watermark_angle(&[1.0, 0.0, 0.0, 1.0, 0.0, 0.0]));
        // 竖排正文：90° / 270° → 保留
        assert!(!ctm_is_watermark_angle(&ctm(90.0)));
        assert!(!ctm_is_watermark_angle(&ctm(270.0)));
        // 反向横排：180° → 保留
        assert!(!ctm_is_watermark_angle(&ctm(180.0)));
        // 斜排水印：30° / 45° → 跳过
        assert!(ctm_is_watermark_angle(&ctm(30.0)));
        assert!(ctm_is_watermark_angle(&ctm(45.0)));
        // 容差边界：12° 内保留、超过 12° 跳过
        assert!(!ctm_is_watermark_angle(&ctm(12.0)));
        assert!(ctm_is_watermark_angle(&ctm(13.0)));
        // 360↔0 环绕边界：348°(= -12°) 保留、347° 跳过
        assert!(!ctm_is_watermark_angle(&ctm(348.0)));
        assert!(ctm_is_watermark_angle(&ctm(347.0)));
        // 畸形/缺省 CTM → 视为轴对齐保留
        assert!(!ctm_is_watermark_angle(&[1.0, 0.0]));
        assert!(!ctm_is_watermark_angle(&[]));
    }

    #[test]
    fn garbled_text_detection() {
        // 正常中文文本 → 不乱码
        let ok = vec![
            OfdTextLine {
                x0: 0.0,
                x1: 10.0,
                y0: 0.0,
                y1: 1.0,
                text: "太原市人民政府公报".to_string(),
            },
            OfdTextLine {
                x0: 0.0,
                x1: 10.0,
                y0: 1.0,
                y1: 2.0,
                text: "二〇二五年第一期".to_string(),
            },
        ];
        assert!(!is_garbled_text(&ok));
        // 60 个 U+FFFD 替换符（>50 字符且占比 100%）→ 乱码
        let bad: Vec<OfdTextLine> = (0..60)
            .map(|i| OfdTextLine {
                x0: 0.0,
                x1: 10.0,
                y0: i as f64,
                y1: i as f64 + 1.0,
                text: "\u{FFFD}".to_string(),
            })
            .collect();
        assert!(is_garbled_text(&bad));
        // 仅 10 个替换符（总量不足 50）→ 不判乱码
        assert!(!is_garbled_text(&bad[..10]));
        // 私有区字符（目录点线符常见）占比 <20%（10 坏 / 70 总）→ 不判乱码
        let mut mixed: Vec<OfdTextLine> = (0..60)
            .map(|i| OfdTextLine {
                x0: 0.0,
                x1: 10.0,
                y0: i as f64,
                y1: i as f64 + 1.0,
                text: "正常正文".to_string(),
            })
            .collect();
        for m in mixed.iter_mut().take(10) {
            m.text = "\u{E000}".to_string();
        }
        assert!(!is_garbled_text(&mixed));
    }

    #[test]
    fn title_prefix_applied() {
        let lines = vec![
            "一、总则".to_string(),
            "这是正文句子。".to_string(),
            "# 已带前缀的标题".to_string(),
        ];
        let out = crate::text_health::apply_title_prefixes(&lines, &[], true);
        assert_eq!(out[0], "## 一、总则");
        assert_eq!(out[1], "这是正文句子。");
        assert_eq!(out[2], "# 已带前缀的标题");
    }
}
