//! OCR 分析引擎单例：模型按 `(tier, layout)` 缓存，跨文档/跨调用复用，免重复加载。
//!
//! `OARStructureBuilder` 经审计确认 `Sync`：底层 `oar_ocr_core::OrtInfer` 用
//! `Vec<Mutex<Session>>` 会话池持有 ORT `Session`（每模型单 session），`Mutex`/`AtomicUsize`
//! 均 `Sync`。T03 已跨 rayon 线程共享 `&analyzer` 且通过 OCR golden——并发推理安全，
//! ORT 内部 intra-op 线程池在单次 `run` 内并行，不依赖我们在外层的多 session。
//!
//! 缓存即**有意常驻**：大文档后不释放模型内存是预期行为（省重载耗时）。`clear_cache()`
//! 提供释放口，防"优化变泄漏"反噬（仅弃缓存自身的 Arc 引用，外部仍持有的引擎句柄有效）。
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex};

use anyhow::Result;
use image::RgbImage;
use oar_ocr::oarocr::{OARStructure, OARStructureBuilder};
use rayon::prelude::*;

use crate::models::{spec_for, OcrLayout, OcrTier};

/// 缓存键：模型档 + 版面模型。热切换 tier/layout 必须用不同 session，故两者都进 key。
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct EngineKey {
    tier: OcrTier,
    layout: OcrLayout,
}

/// 进程级 OCR 引擎缓存（同 key 只建一次模型）。
static CACHE: LazyLock<Mutex<HashMap<EngineKey, Arc<OcrEngine>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 已构建的 OCR 分析器句柄（内部 `Arc` 共享，跨线程安全）。
pub struct OcrEngine {
    analyzer: Arc<OARStructure>,
}

impl OcrEngine {
    /// 按 `(tier, layout)` 取/建引擎。首次命中才下载+构建 ONNX 模型（缓存于 $OAR_HOME），
    /// 后续同 key 零重载——OFD 双 OCR 调用、库模式重复 convert 均复用同实例。
    pub fn build(tier: OcrTier, layout: OcrLayout) -> Result<Arc<OcrEngine>> {
        let key = EngineKey { tier, layout };
        let mut cache = CACHE.lock().expect("ocr engine cache poisoned");
        if let Some(e) = cache.get(&key) {
            return Ok(Arc::clone(e));
        }
        let engine = Arc::new(OcrEngine {
            analyzer: Arc::new(build_analyzer(tier, layout)?),
        });
        // 注意：build_analyzer 返回 OARStructure（predict_images 在其上），非 Builder。
        cache.insert(key, Arc::clone(&engine));
        Ok(engine)
    }

    /// 对一组页面图跑 OCR，返回每页 `StructureResult`（页序保序，契约由断言守恒）。
    /// `threads` 控制**页级并发**：多页切成 chunk 用 rayon 并行 `predict_images`，
    /// 共享 `&self.analyzer`（已证 `Sync`）。
    pub fn predict(
        &self,
        images: Vec<RgbImage>,
        threads: usize,
    ) -> Result<Vec<oar_ocr::domain::structure::StructureResult>> {
        if images.is_empty() {
            return Ok(Vec::new());
        }
        let n = images.len();
        let threads = threads.max(1);
        let chunk_size = n.div_ceil(threads);
        let per_chunk: Vec<Vec<_>> = images
            .into_par_iter()
            .chunks(chunk_size)
            .map(|chunk| self.analyzer.predict_images(chunk))
            .collect();

        let mut out = Vec::with_capacity(n);
        for group in per_chunk {
            for r in group {
                out.push(r.map_err(|e| anyhow::anyhow!("OCR 推理失败: {e}"))?);
            }
        }
        if out.len() != n {
            // 库模式不 panic 宿主：页序契约破坏改为显式 Err（CLI 会打印退出，库调用方可捕获）。
            return Err(anyhow::anyhow!(
                "OCR 输出页数 {} != 输入 {}（页序契约破坏）",
                out.len(),
                n
            ));
        }
        Ok(out)
    }

    /// 释放全部缓存的引擎（仅弃缓存自身 Arc 引用；外部仍持有的句柄继续有效）。
    /// 长驻服务需要周期性回收模型内存时调用。
    pub fn clear_cache() {
        let mut cache = CACHE.lock().expect("ocr engine cache poisoned");
        cache.clear();
    }
}

/// 构建 oar-ocr 分析器：版面模型按 `layout` 选（Doc 默认文档结构 / Table 表格专用），
/// 其余 OCR/表格模型取自 `spec_for(tier)`。返回 `OARStructure`（`predict_images` 在其上）。
fn build_analyzer(tier: OcrTier, layout: OcrLayout) -> Result<OARStructure> {
    let spec = spec_for(tier);
    let (layout_model, layout_name) = match layout {
        OcrLayout::Doc => (spec.layout, spec.layout_name),
        OcrLayout::Table => ("picodet_layout_1x_table.onnx", "PicoDet-Layout-1x-Table"),
    };
    OARStructureBuilder::new(layout_model)
        .layout_model_name(layout_name)
        .with_ocr(spec.det, spec.rec, spec.dict)
        // 表格结构识别（轻量：slanet_plus + 分类 + 字典）
        .with_table_classification(spec.table_cls)
        // 通用结构适配器：Wired/Wireless/Unknown 三分支无专用 adapter 时均回退到它，
        // 避免 table_cls 分类为 Wired 时因无 wired adapter 触发 config_error 整页失败
        .with_table_structure_recognition(spec.table_structure, "wireless")
        .table_structure_dict_path(spec.table_dict)
        .build()
        .map_err(|e| anyhow::anyhow!("构建 OCR 分析器失败: {e}"))
}

/// 兼容旧调用面：等价 `OcrEngine::build(tier, layout)?.predict(images, threads)`。
/// PDF/OFD 两通路继续用此签名，零改动接入单例缓存。
pub fn ocr_images(
    images: Vec<RgbImage>,
    tier: OcrTier,
    layout: OcrLayout,
    threads: usize,
) -> Result<Vec<oar_ocr::domain::structure::StructureResult>> {
    OcrEngine::build(tier, layout)?.predict(images, threads)
}

/// 便捷：对磁盘 PDF 渲染指定页并跑 OCR（库/CLI 共用的"渲染+OCR"一站式入口）。
/// `page_indices` 为 0 基准 pdfium 页号；空 → 全页。接 T07 懒惰渲染。
pub fn ocr_pdf_pages(
    path: &Path,
    dpi: f32,
    tier: OcrTier,
    layout: OcrLayout,
    threads: usize,
    page_indices: &[u32],
) -> Result<Vec<oar_ocr::domain::structure::StructureResult>> {
    let images = crate::pdf::render::render_pdf_pages(path, dpi, page_indices)?;
    OcrEngine::build(tier, layout)?.predict(images, threads)
}
