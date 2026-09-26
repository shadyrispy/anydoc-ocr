//! OFD 渲染（P1.7 自 mod.rs 切分）：整页光栅化 + ADR-0008 ImageObject 直提。
//!
//! 两条取图路径：`render_page` 整页渲染（混合页/直提失败兜底）、
//! `try_extract_ofd_page_image` 单图满页直提原始图像字节（跳过光栅化）。

use std::fs::File;

use image::{RgbImage, RgbaImage};
use ofd_core::model::graphics::{ImageObject, PageBlock};
use ofd_core::{LoadedDocument, OfdReader, RenderOptions, parent_dir, resolve_path};

use crate::ConvertRequest;
use crate::error::{Result as CResult, Stage, runtime};

/// ADR-0008：尝试直提 OFD 页面 ImageObject 的原始图像字节。
///
/// 仅当页面"单图满页"（恰好 1 个 ImageObject、无 TextObject/PathObject）时直提。
/// 路径：`ImageObject.resource_id` → 遍历 Res 找匹配 `CtMultiMedia` →
/// `media_file` → `package.read()` 拿字节 → `image::load_from_memory` 解码。
///
/// 路径解析参考 ofd-core render.rs `load_res_into`：
/// `res_path = resolve_path(doc.base, res_loc)` →
/// `data_base = resolve_path(parent_dir(res_path), res.base_loc)` →
/// `media_path = resolve_path(data_base, mm.media_file)`。
pub(crate) fn try_extract_ofd_page_image(
    reader: &mut OfdReader<File>,
    doc: &LoadedDocument,
    page_idx: usize,
    dpi: f32,
) -> Option<RgbImage> {
    let page_ref = doc.pages().get(page_idx)?;
    let page = reader.load_page(doc, page_ref).ok()?;
    let mut images: Vec<&ImageObject> = Vec::new();
    let mut other_count = 0usize;
    if let Some(content) = &page.content {
        for layer in &content.layers {
            collect_image_objects(&layer.objects, &mut images, &mut other_count);
        }
    }
    if other_count > 0 || images.len() != 1 {
        return None; // 混合页/多图块/无图 → 回退渲染
    }
    let resource_id = images[0].resource_id.value();
    // 遍历文档级 + 页级资源找匹配的 MultiMedia
    let res_locs: Vec<ofd_core::StLoc> = doc
        .public_res()
        .iter()
        .chain(doc.document_res().iter())
        .chain(page.page_res.iter())
        .cloned()
        .collect();
    for loc in &res_locs {
        let res_path = resolve_path(&doc.base, loc);
        let Ok(res) = reader.package_mut().parse::<ofd_core::Res>(&res_path) else {
            continue;
        };
        let res_dir = parent_dir(&res_path).to_string();
        let data_base = resolve_path(&res_dir, &res.base_loc);
        for mm in res.multi_medias() {
            if mm.id.value() == resource_id {
                let media_path = resolve_path(&data_base, &mm.media_file);
                if let Ok(bytes) = reader.package_mut().read(&media_path) {
                    if let Ok(img) = image::load_from_memory(&bytes) {
                        return Some(crate::pdf::render::downscale_to_dpi(img.to_rgb8(), dpi));
                    }
                }
                return None; // 找到资源但读取/解码失败 → 回退渲染
            }
        }
    }
    None
}

/// 递归收集 ImageObject 引用 + 统计非 ImageObject 的 PageBlock 数量。
fn collect_image_objects<'a>(
    blocks: &'a [PageBlock],
    images: &mut Vec<&'a ImageObject>,
    other_count: &mut usize,
) {
    for b in blocks {
        match b {
            PageBlock::Image(img) => images.push(img),
            PageBlock::Block(g) => collect_image_objects(&g.objects, images, other_count),
            _ => *other_count += 1,
        }
    }
}

/// 渲染一页为 RGB 图（图片型分支同参；`dpi` 控制分辨率）。
pub(crate) fn render_page(
    reader: &mut OfdReader<File>,
    doc: &LoadedDocument,
    idx: usize,
    opts: &ConvertRequest,
) -> CResult<RgbImage> {
    let img: RgbaImage = reader
        .render_page_to_image(doc, idx, &RenderOptions::with_dpi(opts.render.dpi.into()))
        .map_err(|e| {
            runtime(
                Stage::Render,
                Some(idx),
                format!("渲染 OFD 第 {idx} 页失败: {e}"),
            )
        })?;
    Ok(image::DynamicImage::ImageRgba8(img).to_rgb8())
}
