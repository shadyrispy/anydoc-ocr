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
use crate::timing::StageTimer;
use crate::{ConvertOptions, Result as CResult};

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
            let text_len: usize = texts.iter().map(|(_, _, s)| s.chars().count()).sum();
            let img_count = count_images(&page);
            let is_image = opts.ofd_force_ocr || (text_len < 5 && img_count > 0);

            if is_image {
                let img: RgbaImage = reader
                    .render_page_to_image(&doc, idx, &RenderOptions::with_dpi(opts.dpi.into()))
                    .map_err(|e| anyhow::anyhow!("渲染 OFD 第 {idx} 页失败: {e}"))?;
                let rgb = image::DynamicImage::ImageRgba8(img).to_rgb8();
                let slot = out_pages.len();
                out_pages.push(String::new());
                img_batch.push((slot, rgb));
            } else {
                // 按 (y, x) 排序：同一行（同 y）的多个 TextObject 按 x 次级排序保证顺序
                let mut t = texts;
                t.sort_by(|a, b| {
                    a.0.partial_cmp(&b.0)
                        .unwrap_or(Ordering::Equal)
                        .then(a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal))
                });
                let md = t
                    .into_iter()
                    .map(|(_, _, s)| s)
                    .collect::<Vec<_>>()
                    .join("\n");
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

/// 收集一页所有 TextObject 的文本，返回 (y, x, 行文本)。
fn collect_text_lines(page: &PageObject) -> Vec<(f64, f64, String)> {
    let mut out = Vec::new();
    if let Some(content) = &page.content {
        for layer in &content.layers {
            collect_text_blocks(&layer.objects, &mut out);
        }
    }
    out
}

fn collect_text_blocks(blocks: &[PageBlock], out: &mut Vec<(f64, f64, String)>) {
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
                let y = t.text_codes.first().and_then(|c| c.y).unwrap_or(0.0);
                let x = t.text_codes.first().and_then(|c| c.x).unwrap_or(0.0);
                if !line.trim().is_empty() {
                    out.push((y, x, line));
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
