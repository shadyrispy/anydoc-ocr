//! OFD 通道：文字型走 ofd-core 文本提取；图片型走渲染+OCR 管线
//!
//! 页型判定：逐页统计文本量，低于阈值且存在图像对象则视为图片型（或
//! `--ofd-force-ocr` 强制），走与 PDF 共用的 OCR 回退管线；否则按坐标提取
//! TextObject 文本流，保持与 pdf-inspector 风格一致的纯文本 GFM。
use std::cmp::Ordering;
use std::path::Path;

use image::{RgbImage, RgbaImage};
use ofd_core::model::graphics::PageBlock;
use ofd_core::model::page::PageObject;
use ofd_core::{OfdReader, RenderOptions};

use crate::gfm_adapter;
use crate::pdf::ocr;
use crate::reading_order;
use crate::timing::StageTimer;
use crate::{ConvertOptions, Result as CResult};

/// 页型判定阈值：文字总量（字符数）低于该值且存在图像对象时视为图片型页，
/// 走渲染+OCR；否则按坐标提取文字层（与 `--ofd-force-ocr` 无关的默认判定）。
const IMAGE_PAGE_MIN_TEXT_CHARS: usize = 5;

/// OFD → Markdown 总入口。
pub fn convert_ofd(path: &Path, opts: &ConvertOptions) -> CResult<String> {
    let mut t = StageTimer::new();
    let mut reader =
        OfdReader::open(path).map_err(|e| anyhow::anyhow!("打开 OFD 失败: {e}"))?;
    // clone 出来避免遍历时与 reader 的 &mut 借用冲突
    let doc_bodies = reader.ofd().doc_bodies.clone();

    let mut out_pages: Vec<String> = Vec::new();
    // 图片型页面先占位并记录槽位，渲染图累积后一次性 OCR（享页级并行）
    let mut img_batch: Vec<(usize, RgbImage)> = Vec::new();

    for body in &doc_bodies {
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
            let text_len: usize = texts.iter().map(|(_, _, _, _, s)| s.chars().count()).sum();
            let img_count = count_images(&page);
            let is_image = opts.ofd_force_ocr
                || (text_len < IMAGE_PAGE_MIN_TEXT_CHARS && img_count > 0);

            if is_image {
                let img: RgbaImage = reader
                    .render_page_to_image(&doc, idx, &RenderOptions::with_dpi(opts.dpi.into()))
                    .map_err(|e| anyhow::anyhow!("渲染 OFD 第 {idx} 页失败: {e}"))?;
                let rgb = image::DynamicImage::ImageRgba8(img).to_rgb8();
                let slot = out_pages.len();
                out_pages.push(String::new());
                img_batch.push((slot, rgb));
            } else {
                // 双列/多列阅读顺序：复用共享 `reading_order`（PDF 文字层同一算法）。
                // 每行 TextObject 是完整一行，区域直接用其真实页面包围盒
                // （x_min/max、y_min/max 来自 boundary，见 `collect_text_lines`），
                // 使跨整页的页眉/页脚（如"太原市人民政府公报 + 页码"）能命中
                // reading_order 的 is_full 判定，提前到正文之前而非按中心 x 落入
                // 右列；boundary 退化（宽/高非法）时已退回单点区域。
                let regions: Vec<(f32, f32, f32, f32, String)> = texts
                    .into_iter()
                    .map(|(x0, x1, y0, y1, s)| (x0 as f32, x1 as f32, y0 as f32, y1 as f32, s))
                    .collect();
                let md = reading_order::order_text_regions(&regions).join("\n");
                out_pages.push(md);
            }
        }
    }

    if !img_batch.is_empty() {
        let images: Vec<RgbImage> = img_batch.iter().map(|(_, img)| img.clone()).collect();
        t.stage("render");
        let results = ocr::ocr_images(images, opts.ocr_tier, opts.ocr_layout, opts.threads)?;
        t.stage("ocr");
        for (i, (slot, _)) in img_batch.iter().enumerate() {
            out_pages[*slot] =
                gfm_adapter::structure_results_to_gfm(std::slice::from_ref(&results[i]));
        }
    }
    t.stage("gfm");
    Ok(out_pages.join("\n\n"))
}

/// 收集一页所有 TextObject 的文本，返回 `(x_min, x_max, y_min, y_max, 行文本)`，
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
fn collect_text_lines(page: &PageObject) -> Vec<(f64, f64, f64, f64, String)> {
    let mut out = Vec::new();
    if let Some(content) = &page.content {
        for layer in &content.layers {
            collect_text_blocks(&layer.objects, &mut out);
        }
    }
    out
}

fn collect_text_blocks(blocks: &[PageBlock], out: &mut Vec<(f64, f64, f64, f64, String)>) {
    for b in blocks {
        match b {
            PageBlock::Text(t) => {
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
                    out.push((x0, x1, y0, y1, line));
                } else {
                    // boundary 退化：退回旧单点行为（首字符坐标），不 panic。
                    out.push((x, x, y, y + 1.0, line));
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
