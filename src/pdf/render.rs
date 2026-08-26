//! PDF 渲染：PDFium 逐页渲染为 RGB 图像（供 OCR 管线）
//!
//! ADR-0008：图片型 PDF 优先直提 image object 原始像素（`image_data()`），
//! 跳过整页光栅化——避免 MediaBox 异常时渲染放大导致 OOM，且省去重复光栅化。
//! 仅"单图满页"（恰好 1 个 image object、无 text/path/shading）才直提；
//! 混合页/多图块回退 `page.render()`。
use std::collections::BTreeSet;
use std::path::Path;

use crate::error::{Result, Stage, runtime};
use pdfium_render::prelude::*;

/// 将 PDF 指定页按 `dpi` 渲染为 `RgbImage`。
///
/// `page_indices` 为 0 基准 pdfium 页号；**空切片 → 渲染全部页**（图片型 PDF 全量路径）。
/// 仅渲子集（非全量）时峰值内存大降——52p 文档只渲 3 个可疑页，而非 52 页全物化。
/// 输出顺序 = 升序命中的页号（与调用方 `suspicious` 升序 zip 保持锁步）。
///
/// libpdfium.so 定位优先级：`PDFIUM_LIB_DIR` 环境变量 > 可执行文件旁 `lib/`（打包布局）> 开发期相对路径。
///
/// ADR-0006：错误类型 `ConvertError`，pdfium 绑定/渲染失败归
/// `Malformed { part: "page N" | None, detail }`（运行时错误，非文档本身问题）。
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
            return Err(runtime(
                Stage::Render,
                None,
                format!("绑定 libpdfium.so 失败（设置 PDFIUM_LIB_DIR 或随包 lib/）: {so}: {e}"),
            ));
        }
    };

    let doc = pdfium.load_pdf_from_file(path, None).map_err(|e| {
        runtime(
            Stage::Render,
            None,
            format!("PDFium 加载 PDF 失败: {}: {e}", path.display()),
        )
    })?;

    // 空 = 全渲；非空 = 仅渲指定索引（排序去重后集合，O(1) 判定）
    let target: Option<BTreeSet<u32>> = if page_indices.is_empty() {
        None
    } else {
        Some(page_indices.iter().copied().collect())
    };
    render_document(&doc, dpi, &target)
}

/// ADR-0008：尝试直提页面 image object 的原始像素。
///
/// 仅当页面"单图满页"（恰好 1 个 image object、无 text/path/shading object）时
/// 直提——图片型扫描件的典型形态。混合页/多图块返回 `None`，调用方回退整页渲染。
///
/// 直提跳过 pdfium 的整页光栅化（`page.render`），避免：
/// 1. MediaBox 异常（如扫描仪把像素值当 pt）时渲染放大导致 OOM
/// 2. 对已解码图像的重复光栅化（浪费 CPU + 内存）
///
/// `get_raw_image()` 返回 image object 解码后的原始像素（考虑 filter/mask/transform），
/// 非 MediaBox×DPI 缩放的渲染结果。
///
/// **降采样**：扫描件原始像素可能远超 OCR 所需（如 460dpi 扫描 → 3609×2540），
/// 多页并发 OCR 会撑爆 cgroup 内存。直提后按 `dpi` 对应的 A4 长边（~12in×dpi）
/// 限制最长边，超过则 Lanczos 降采样——既控内存又对齐 PP-OCR 训练分布
/// （ADR-0007 实测 tiny/200dpi 反退化，100dpi 最优）。
fn try_extract_page_image(page: &PdfPage, dpi: f32) -> Option<image::RgbImage> {
    let mut image_count = 0usize;
    let mut path_count = 0usize;
    let mut other_count = 0usize;
    let mut result = None;
    for obj in page.objects().iter() {
        match obj.object_type() {
            PdfPageObjectType::Image => {
                image_count += 1;
                if image_count > 1 {
                    return None; // 多个 image object → 回退渲染
                }
                result = obj
                    .as_image_object()
                    .and_then(|io| io.get_raw_image().ok())
                    .map(|i| i.to_rgb8());
            }
            PdfPageObjectType::Text => {} // 隐藏文字层（OCR 生成），不影响图像
            PdfPageObjectType::Path => path_count += 1,
            _ => other_count += 1,
        }
    }
    if image_count != 1 || path_count > 0 || other_count > 0 {
        return None; // 无图/多图/混合页（含 Path 等可见矢量）→ 回退渲染
    }
    result.map(|img| downscale_to_dpi(img, dpi))
}

/// ADR-0005 候选 2：跨文档渲染闭包——在专属线程内逐 doc open + 逐页产出
/// ((doc_idx, page_idx), img) 入 channel。文档边界不停顿，OCR 池跨文档消费。
///
/// 单文档调用方传 `vec![path]`（doc_idx 恒 0）；多文档批处理传完整 paths 列表，
/// doc_idx 与 paths 索引一一对应。
///
/// 单个 PDF 打开失败 → 告警跳过该 doc（其页缺失，调用方容错），不中断整批。
///
/// ADR-0006：错误类型 `ConvertError`，渲染失败归 `Malformed { part: "page N", detail }`。
/// ADR-0008：优先直提 image object（`try_extract_page_image`），失败回退整页渲染。
pub fn render_cross_doc_fn(
    paths: Vec<std::path::PathBuf>,
    dpi: f32,
) -> impl FnOnce(
    std::sync::mpsc::SyncSender<super::super::pipeline::RenderItem>,
) -> crate::error::Result<()>
+ Send
+ 'static {
    render_docs_filtered(paths, dpi, Box::new(|_, _| true))
}

/// 按页子集渲染（T2 按页重试用）：只渲染 `subset` 指定的 `(doc_idx, page_idx)` 页，
/// 供更高档局部重跑失败页。复用与全量渲染同一核心循环（`render_docs_filtered`），
/// 保证直提 image object / 回退光栅化 / 错误结构化等语义一致。
pub fn render_cross_doc_subset_fn(
    paths: Vec<std::path::PathBuf>,
    dpi: f32,
    subset: Vec<(usize, usize)>,
) -> impl FnOnce(
    std::sync::mpsc::SyncSender<super::super::pipeline::RenderItem>,
) -> crate::error::Result<()>
+ Send
+ 'static {
    let keep: std::collections::HashSet<(usize, usize)> = subset.into_iter().collect();
    render_docs_filtered(paths, dpi, Box::new(move |d, p| keep.contains(&(d, p))))
}

/// 渲染核心：逐 doc open + 逐页渲染，`keep(doc_idx, page_idx)` 为 false 的页跳过。
/// 被 [`render_cross_doc_fn`]（全量）与 [`render_cross_doc_subset_fn`]（子集）共用，
/// 保证两种路径的 PDFium 绑定、直提/光栅化回退、错误结构化完全一致。
fn render_docs_filtered(
    paths: Vec<std::path::PathBuf>,
    dpi: f32,
    keep: Box<dyn Fn(usize, usize) -> bool + Send + Sync>,
) -> impl FnOnce(
    std::sync::mpsc::SyncSender<super::super::pipeline::RenderItem>,
) -> crate::error::Result<()>
+ Send
+ 'static {
    move |tx| {
        let so = locate_pdfium()?;
        let pdfium = match Pdfium::bind_to_library(&so) {
            Ok(bindings) => Pdfium::new(bindings),
            Err(PdfiumError::PdfiumLibraryBindingsAlreadyInitialized) => Pdfium::default(),
            Err(e) => {
                return Err(runtime(
                    Stage::Render,
                    None,
                    format!("绑定 libpdfium.so 失败（设置 PDFIUM_LIB_DIR 或随包 lib/）: {so}: {e}"),
                ));
            }
        };
        let scale = dpi / 72.0;
        for (doc_idx, path) in paths.iter().enumerate() {
            let doc = match pdfium.load_pdf_from_file(path, None) {
                Ok(d) => d,
                Err(e) => {
                    // 整文档打开失败 → 结构化回传（ADR 候选 3）。用哨兵页号 usize::MAX
                    // 标记"整文档"错误，batch 借此回填真实 detail，而非"详见 stderr"占位。
                    let _ = tx.send(Err((
                        (doc_idx, usize::MAX),
                        runtime(
                            Stage::Render,
                            None,
                            format!("打开 doc {doc_idx}（{}）失败: {e}", path.display()),
                        ),
                    )));
                    continue;
                }
            };
            for (i, page) in doc.pages().iter().enumerate() {
                if !keep(doc_idx, i) {
                    continue;
                }
                // ADR-0008：优先直提 image object（单图满页），跳过整页光栅化。
                // 直提成功 → 直接送 OCR；失败（混合页/多图块/解码错误）→ 回退渲染。
                if let Some(img) = try_extract_page_image(&page, dpi) {
                    if std::env::var_os("ANYDOC_RENDER_TRACE").is_some() {
                        let (w, h) = img.dimensions();
                        eprintln!("[render] doc{doc_idx} p{i} → 直提 image object ({w}x{h})");
                    }
                    if tx.send(Ok(((doc_idx, i), img))).is_err() {
                        return Ok(()); // OCR 端退出
                    }
                    continue;
                }
                if std::env::var_os("ANYDOC_RENDER_TRACE").is_some() {
                    eprintln!("[render] doc{doc_idx} p{i} → 回退 PDFium 整页渲染");
                }
                let w = (page.width().value * scale) as i32;
                let h = (page.height().value * scale) as i32;
                if w <= 0 || h <= 0 {
                    let _ = tx.send(Err((
                        (doc_idx, i),
                        runtime(
                            Stage::Render,
                            Some(i),
                            format!("PDF doc {doc_idx} 第 {i} 页渲染尺寸异常: w={w} h={h}"),
                        ),
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
                            let _ = tx.send(Err((
                                (doc_idx, i),
                                runtime(
                                    Stage::Render,
                                    Some(i),
                                    format!("doc {doc_idx} bitmap 转 image 失败: {e}"),
                                ),
                            )));
                        }
                    },
                    Err(e) => {
                        let _ = tx.send(Err((
                            (doc_idx, i),
                            runtime(
                                Stage::Render,
                                Some(i),
                                format!("渲染 doc{doc_idx} 第 {i} 页失败: {e}"),
                            ),
                        )));
                    }
                }
            }
        }
        Ok(())
    }
}

/// ADR-0008：按 DPI 降采样图像到 A4 长边对应尺寸。
///
/// 扫描件原始像素常远超 OCR 所需（460dpi 扫描 → 3609×2540），多页并发 OCR
/// 会撑爆 cgroup 内存。按 A4 长边 ~12in × `dpi` 限制最长边，超过则 Lanczos
/// 降采样。低于上限原样返回（避免无谓拷贝）。
///
/// `dpi` 与渲染路径语义一致：100 → 最长边 ~1200px。PP-OCR 训练分布在此范围
/// （ADR-0007 实测 100dpi 最优，200dpi 反退化）。
pub(crate) fn downscale_to_dpi(img: image::RgbImage, dpi: f32) -> image::RgbImage {
    let max_side = (dpi * 12.0).round().max(1.0) as u32;
    let (w, h) = img.dimensions();
    let longest = w.max(h);
    if longest <= max_side {
        return img; // 已在目标范围内，无需降采样
    }
    let scale = max_side as f32 / longest as f32;
    let new_w = ((w as f32 * scale).round() as u32).max(1);
    let new_h = ((h as f32 * scale).round() as u32).max(1);
    image::imageops::resize(&img, new_w, new_h, image::imageops::FilterType::Lanczos3)
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
    Err(runtime(
        Stage::Render,
        None,
        "找不到 libpdfium.so（设置 PDFIUM_LIB_DIR 或将其置于可执行文件旁 lib/）",
    ))
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
            return Err(runtime(
                Stage::Render,
                Some(i),
                format!("PDF 第 {i} 页渲染尺寸异常: w={w} h={h}"),
            ));
        }
        let bitmap = page.render(w, h, None).map_err(|e| {
            runtime(
                Stage::Render,
                Some(i),
                format!("渲染第 {i} 页失败: {e}"),
            )
        })?;
        let img = bitmap.as_image().map_err(|e| {
            runtime(
                Stage::Render,
                Some(i),
                format!("bitmap 转 image 失败: {e}"),
            )
        })?;
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
