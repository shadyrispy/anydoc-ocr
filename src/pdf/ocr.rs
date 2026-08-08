//! OCR 装配：用 oar-ocr 对渲染图做版面+文本+表格分析
use anyhow::Result;
use image::RgbImage;
use rayon::prelude::*;

use crate::models::{spec_for, OcrLayout, OcrTier};
use oar_ocr::oarocr::OARStructureBuilder;

/// 对一组页面图像跑 OCR 管线，返回每页 StructureResult。
///
/// 模型按 `tier` 选择，首次运行经 auto-download 从 ModelScope 拉取（缓存于 $OAR_HOME）。
/// `layout` 控制版面模型：`Doc` 默认文档结构；`Table` 换表格专用版面
/// （`picodet_layout_1x_table.onnx`，只标 Table，检出才跑 SLANet，无表页零表格开销）。
/// `threads` 控制**页级并发**：多页时切成若干 chunk，用 rayon 并行 `predict_images`，
/// 单页或 `threads<=1` 走原生调用（保留 oar-ocr 内部 batching）。
/// 注：oar-ocr/ORT 的 intra-op 线程池不可外部调参，本旋钮控制的是"同时推理的页数"。
pub fn ocr_images(
    images: Vec<RgbImage>,
    tier: OcrTier,
    layout: OcrLayout,
    threads: usize,
) -> Result<Vec<oar_ocr::domain::structure::StructureResult>> {
    if images.is_empty() {
        return Ok(Vec::new());
    }

    let spec = spec_for(tier);
    let (layout_model, layout_name) = match layout {
        OcrLayout::Doc => (spec.layout, spec.layout_name),
        OcrLayout::Table => ("picodet_layout_1x_table.onnx", "PicoDet-Layout-1x-Table"),
    };
    let analyzer = OARStructureBuilder::new(layout_model)
        .layout_model_name(layout_name)
        .with_ocr(spec.det, spec.rec, spec.dict)
        // 表格结构识别（轻量：slanet_plus + 分类 + 字典）
        .with_table_classification(spec.table_cls)
        // 通用结构适配器：Wired/Wireless/Unknown 三分支无专用 adapter 时均回退到它，
        // 避免 table_cls 分类为 Wired 时因无 wired adapter 触发 config_error 整页失败
        .with_table_structure_recognition(spec.table_structure, "wireless")
        .table_structure_dict_path(spec.table_dict)
        .build()
        .map_err(|e| anyhow::anyhow!("构建 OCR 分析器失败: {e}"))?;

    let threads = threads.max(1);
    let chunk_size = (images.len() + threads - 1) / threads;
    let chunks: Vec<Vec<RgbImage>> = images
        .chunks(chunk_size)
        .map(|c| c.to_vec())
        .collect();

    let per_chunk: Vec<Vec<_>> = chunks
        .into_par_iter()
        .map(|chunk| analyzer.predict_images(chunk))
        .collect();

    let mut out = Vec::with_capacity(images.len());
    for group in per_chunk {
        for r in group {
            out.push(r.map_err(|e| anyhow::anyhow!("OCR 推理失败: {e}"))?);
        }
    }
    Ok(out)
}
