//! ORT 后端：封装 oar-ocr 的 OARStructureBuilder + rayon 页级并行
use image::RgbImage;
use oar_ocr::oarocr::{OARStructure, OARStructureBuilder};
use rayon::prelude::*;

use crate::models::{ModelSpec, OcrLayout};
use crate::ocr::OcrEngine;
use crate::Error;

/// ORT 引擎：持有已构建的 analyzer（OARStructure），predict 时按 threads 切 chunk 并行
pub struct OrtEngine {
    analyzer: OARStructure,
    threads: usize,
}

impl OrtEngine {
    pub fn new(spec: &ModelSpec, layout: OcrLayout, threads: usize) -> crate::Result<Self> {
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
            .map_err(|e| Error::Other(anyhow::anyhow!("构建 OCR 分析器失败: {e}")))?;
        Ok(OrtEngine { analyzer, threads: threads.max(1) })
    }
}

impl OcrEngine for OrtEngine {
    fn predict(
        &self,
        images: &[RgbImage],
    ) -> crate::Result<Vec<oar_ocr::domain::structure::StructureResult>> {
        if images.is_empty() {
            return Ok(Vec::new());
        }
        let chunk_size = (images.len() + self.threads - 1) / self.threads;
        let chunks: Vec<&[RgbImage]> = images.chunks(chunk_size).collect();

        let per_chunk: Vec<Vec<_>> = chunks
            .into_par_iter()
            .map(|chunk| self.analyzer.predict_images(chunk.to_vec()))
            .collect();

        let mut out = Vec::with_capacity(images.len());
        for group in per_chunk {
            for r in group {
                out.push(r.map_err(|e| Error::Other(anyhow::anyhow!("OCR 推理失败: {e}")))?);
            }
        }
        Ok(out)
    }
}
