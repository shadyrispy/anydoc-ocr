//! PDF 通道：文字型走 anydoc（字节级一致）；图片型走 OCR 管线（M1）
use std::path::Path;

use crate::{gfm_adapter, timing::StageTimer, ConvertOptions, Result};

pub mod ocr;
pub mod render;

pub fn convert_pdf(path: &Path, opts: &ConvertOptions) -> Result<String> {
    let mut t = StageTimer::new();
    // 文字型：anydoc 字节级一致输出（非空文本即视为文字型）；
    // 除非 --pdf-force-ocr（把文字型 PDF 当图片渲染后 OCR，用于图片型校准）
    if !opts.pdf_force_ocr {
        if let Ok(md) = anydoc::to_markdown(path) {
            if !md.trim().is_empty() {
                return Ok(md);
            }
        }
    }
    // 图片型：PDFium 渲染 + oar-ocr OCR
    // DPI 默认 100（可由 --dpi 调整）。DPI 200→100：像素量降 75%，实测 上海公报52p
    // 148.5s→100.0s(-33%)，内容恢复率零损失(99.83%)；80 起脚注/小字开始漏检。
    let images = render::render_pdf_pages(path, opts.dpi)?;
    t.stage("render");
    let pages = ocr::ocr_images(images, opts.ocr_tier, opts.ocr_layout, opts.threads)?;
    t.stage("ocr");
    let md = gfm_adapter::structure_results_to_gfm(&pages);
    t.stage("gfm");
    Ok(md)
}
