//! PDF 渲染：PDFium 逐页渲染为 RGB 图像（供 OCR 管线）
use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result};
use pdfium_render::prelude::*;

/// 将 PDF 指定页按 `dpi` 渲染为 `RgbImage`。
///
/// `page_indices` 为 0 基准 pdfium 页号；**空切片 → 渲染全部页**（图片型 PDF 全量路径）。
/// 仅渲子集（非全量）时峰值内存大降——52p 文档只渲 3 个可疑页，而非 52 页全物化。
/// 输出顺序 = 升序命中的页号（与调用方 `suspicious` 升序 zip 保持锁步）。
///
/// libpdfium.so 定位优先级：`PDFIUM_LIB_DIR` 环境变量 > 可执行文件旁 `lib/`（打包布局）> 开发期相对路径。
pub fn render_pdf_pages(
    path: &Path,
    dpi: f32,
    page_indices: &[u32],
) -> Result<Vec<image::RgbImage>> {
    let so = locate_pdfium()?;
    // pdfium 绑定是全局单例：文字层预检（pdf-inspector extract_pages_markdown）
    // 已初始化时，这里 bind 返回 AlreadyInitialized——用 Pdfium::default() 复用
    // 既有绑定继续（其内部已处理该错误分支）。
    let pdfium = match Pdfium::bind_to_library(&so) {
        Ok(bindings) => Pdfium::new(bindings),
        Err(PdfiumError::PdfiumLibraryBindingsAlreadyInitialized) => Pdfium::default(),
        Err(e) => {
            anyhow::bail!("绑定 libpdfium.so 失败（设置 PDFIUM_LIB_DIR 或随包 lib/）: {so}: {e}")
        }
    };

    let doc = pdfium
        .load_pdf_from_file(path, None)
        .with_context(|| format!("PDFium 加载 PDF 失败: {}", path.display()))?;

    // 空 = 全渲；非空 = 仅渲指定索引（排序去重后集合，O(1) 判定）
    let target: Option<BTreeSet<u32>> = if page_indices.is_empty() {
        None
    } else {
        Some(page_indices.iter().copied().collect())
    };
    render_document(&doc, dpi, &target)
}

/// ADR-0005 候选 2：跨文档渲染闭包——在专属线程内逐 doc open + 逐页产出
/// ((doc_idx, page_idx), img) 入 channel。文档边界不停顿，OCR 池跨文档消费。
///
/// 单文档调用方传 `vec![path]`（doc_idx 恒 0）；多文档批处理传完整 paths 列表，
/// doc_idx 与 paths 索引一一对应。
///
/// 单个 PDF 打开失败 → 告警跳过该 doc（其页缺失，调用方容错），不中断整批。
pub fn render_cross_doc_fn(
    paths: Vec<std::path::PathBuf>,
    dpi: f32,
) -> impl FnOnce(std::sync::mpsc::SyncSender<super::super::pipeline::RenderItem>) -> anyhow::Result<()> + Send + 'static
{
    move |tx| {
        let so = locate_pdfium()?;
        let pdfium = match Pdfium::bind_to_library(&so) {
            Ok(bindings) => Pdfium::new(bindings),
            Err(PdfiumError::PdfiumLibraryBindingsAlreadyInitialized) => Pdfium::default(),
            Err(e) => {
                anyhow::bail!(
                    "绑定 libpdfium.so 失败（设置 PDFIUM_LIB_DIR 或随包 lib/）: {so}: {e}"
                )
            }
        };
        let scale = dpi / 72.0;
        for (doc_idx, path) in paths.iter().enumerate() {
            let doc = match pdfium.load_pdf_from_file(path, None) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("[pipeline] 跨文档渲染：打开 {} 失败: {e}", path.display());
                    continue;
                }
            };
            for (i, page) in doc.pages().iter().enumerate() {
                let w = (page.width().value * scale) as i32;
                let h = (page.height().value * scale) as i32;
                if w <= 0 || h <= 0 {
                    let _ = tx.send(Err((
                        (doc_idx, i),
                        anyhow::anyhow!("PDF {doc_idx} 第 {i} 页渲染尺寸异常: w={w} h={h}"),
                    )));
                    continue;
                }
                match page.render(w, h, None) {
                    Ok(bitmap) => match bitmap.as_image() {
                        Ok(img) => {
                            if tx.send(Ok(((doc_idx, i), img.to_rgb8()))).is_err() {
                                return Ok(()); // OCR 端退出
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Err(((doc_idx, i), anyhow::anyhow!("bitmap 转 image 失败: {e}"))));
                        }
                    },
                    Err(e) => {
                        let _ = tx.send(Err((
                            (doc_idx, i),
                            anyhow::anyhow!("渲染 doc{doc_idx} 第 {i} 页失败: {e}"),
                        )));
                    }
                }
            }
        }
        Ok(())
    }
}

/// 定位 libpdfium.so：env > 可执行文件旁 lib/ > 开发期相对路径。
fn locate_pdfium() -> Result<String> {
    if let Ok(dir) = std::env::var("PDFIUM_LIB_DIR") {
        let p = std::path::Path::new(&dir).join("libpdfium.so");
        if p.exists() {
            return Ok(p.to_string_lossy().into_owned());
        }
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        let p = parent.join("lib/libpdfium.so");
        if p.exists() {
            return Ok(p.to_string_lossy().into_owned());
        }
    }
    let dev = std::path::Path::new("third_party/pdfium/x64/lib/libpdfium.so");
    if dev.exists() {
        return Ok(dev.to_string_lossy().into_owned());
    }
    anyhow::bail!("找不到 libpdfium.so（设置 PDFIUM_LIB_DIR 或将其置于可执行文件旁 lib/）")
}

fn render_document(
    doc: &PdfDocument,
    dpi: f32,
    target: &Option<BTreeSet<u32>>,
) -> Result<Vec<image::RgbImage>> {
    let scale = dpi / 72.0;
    let mut out = Vec::new();
    for (i, page) in doc.pages().iter().enumerate() {
        // 懒惰渲染：仅 `target` 含本页索引时才渲染，跳过页不物化位图（内存收益在此）
        if let Some(t) = target
            && !t.contains(&(i as u32))
        {
            continue;
        }
        let w = (page.width().value * scale) as i32;
        let h = (page.height().value * scale) as i32;
        if w <= 0 || h <= 0 {
            // 不静默跳过：跳过会使输出图像数 < 请求页数，调用方 `zip(页号)` 错位
            // （表格归属错页）。0 尺寸页本就无法渲染（PDF 规范页尺寸须 >0），
            // 显式报错，由调用方容错回退（文字层）而非产出错位结果。
            anyhow::bail!("PDF 第 {i} 页渲染尺寸异常: w={w} h={h}");
        }
        let bitmap = page
            .render(w, h, None)
            .with_context(|| format!("渲染第 {i} 页失败"))?;
        let img = bitmap.as_image().context("bitmap 转 image 失败")?;
        out.push(img.to_rgb8());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 懒惰渲染子集 == 全量渲染后取同索引子集（尺寸+像素一致）。
    /// 用入库小样本（确定性、非 OCR），缺失则跳过。
    #[test]
    fn lazy_render_subset_equals_full_indexed() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/samples/multipage.pdf");
        if !path.exists() {
            eprintln!("[render] skip: tests/samples/multipage.pdf missing");
            return;
        }
        let dpi = 100.0_f32;
        let full = render_pdf_pages(&path, dpi, &[]).expect("full render");
        assert!(full.len() >= 3, "样本页不足 3 页，无法测子集");
        // 取第 0、2 页（升序）
        let subset_idx: Vec<u32> = vec![0, 2];
        let lazy = render_pdf_pages(&path, dpi, &subset_idx).expect("lazy render");

        assert_eq!(lazy.len(), subset_idx.len(), "输出数须=请求索引数");
        for (k, &idx) in subset_idx.iter().enumerate() {
            let a = &full[idx as usize];
            let b = &lazy[k];
            assert_eq!(a.dimensions(), b.dimensions(), "页 {idx} 尺寸不一致");
            assert_eq!(a.as_raw(), b.as_raw(), "页 {idx} 像素不一致");
        }
    }
}
