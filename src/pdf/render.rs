//! PDF 渲染：PDFium 逐页渲染为 RGB 图像（供 OCR 管线）
use std::path::Path;

use anyhow::{Context, Result};
use pdfium_render::prelude::*;

/// 将 PDF 每页按 `dpi` 渲染为 `RgbImage`。
/// libpdfium.so 定位优先级：`PDFIUM_LIB_DIR` 环境变量 > 可执行文件旁 `lib/`（打包布局）> 开发期相对路径。
pub fn render_pdf_pages(path: &Path, dpi: f32) -> Result<Vec<image::RgbImage>> {
    let so = locate_pdfium()?;
    let bindings = Pdfium::bind_to_library(&so)
        .with_context(|| format!("绑定 libpdfium.so 失败（设置 PDFIUM_LIB_DIR 或随包 lib/）: {so}"))?;
    let pdfium = Pdfium::new(bindings);

    let doc = pdfium
        .load_pdf_from_file(path, None)
        .with_context(|| format!("PDFium 加载 PDF 失败: {}", path.display()))?;

    Ok(render_document(&doc, dpi)?)
}

/// 定位 libpdfium.so：env > 可执行文件旁 lib/ > 开发期相对路径。
fn locate_pdfium() -> Result<String> {
    if let Ok(dir) = std::env::var("PDFIUM_LIB_DIR") {
        let p = std::path::Path::new(&dir).join("libpdfium.so");
        if p.exists() {
            return Ok(p.to_string_lossy().into_owned());
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let p = parent.join("lib/libpdfium.so");
            if p.exists() {
                return Ok(p.to_string_lossy().into_owned());
            }
        }
    }
    let dev = std::path::Path::new("third_party/pdfium/x64/lib/libpdfium.so");
    if dev.exists() {
        return Ok(dev.to_string_lossy().into_owned());
    }
    anyhow::bail!("找不到 libpdfium.so（设置 PDFIUM_LIB_DIR 或将其置于可执行文件旁 lib/）")
}

fn render_document(doc: &PdfDocument, dpi: f32) -> Result<Vec<image::RgbImage>> {
    let scale = dpi / 72.0;
    let mut out = Vec::new();
    for (i, page) in doc.pages().iter().enumerate() {
        let w = (page.width().value * scale) as i32;
        let h = (page.height().value * scale) as i32;
        if w <= 0 || h <= 0 {
            continue;
        }
        let bitmap = page
            .render(w, h, None)
            .with_context(|| format!("渲染第 {i} 页失败"))?;
        let img = bitmap.as_image().context("bitmap 转 image 失败")?;
        out.push(img.to_rgb8());
    }
    Ok(out)
}
