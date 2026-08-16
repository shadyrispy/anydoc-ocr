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
use std::sync::{Arc, LazyLock, Mutex, Once};
use std::time::Instant;

use crate::error::{Result, runtime};
use image::RgbImage;
use oar_ocr::oarocr::{OARStructure, OARStructureBuilder};
use rayon::prelude::*;

use crate::models::{OcrLayout, OcrTier, spec_for};

/// 进程级 ORT 线程池已提交守卫（仅首次 OCR 触达 ORT 前生效一次）。
static ORT_INIT: Once = Once::new();

/// 消除 rayon 页级并行 × ORT 默认 intra 的线程超额订阅（Ticket A）。
///
/// ORT 默认 `intra_threads` = 全核；而 `predict` 又用 rayon 按 `parallel` 页并行，
/// 二者相乘在 8 核飞腾上为 4×8=32 线程风暴。这里提交进程级 ORT 线程池，
/// 令 `intra = max(1, cores / parallel)`，使总线程≈核心数、无超额订阅。
///
/// `parallel` 取自 `opts.threads`；`ANYDOC_ORT_INTRA_THREADS` 可强制覆盖（调试用）。
/// ORT 环境为进程全局、首次初始化者生效；已在别处初始化则本调用被忽略（幂等）。
///
/// **必须在 `OcrEngine::build` 之前调用**：ORT 全局线程池只有先于任何 ONNX session
/// 创建时 commit 才生效，而 session 在 `build` 内的 `build_analyzer` 里创建；放到
/// session 之后（如 `predict` 内）提交将被 ORT 忽略，本配置形同虚设。
pub(crate) fn init_runtime(parallel: usize) {
    ORT_INIT.call_once(|| {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let parallel = parallel.max(1).min(cores);
        let intra = std::env::var("ANYDOC_ORT_INTRA_THREADS")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or_else(|| (cores / parallel).max(1));
        // 配置失败不得掀翻宿主进程（本 crate 亦作为库被集成）：告警后回落 ORT 默认线程池。
        let opts = match ort::environment::GlobalThreadPoolOptions::default()
            .with_intra_threads(intra)
        {
            Ok(o) => o,
            Err(e) => {
                eprintln!(
                    "[anydoc-ocr] 警告：ORT intra_threads={intra} 配置失败（{e}），回落 ORT 默认线程池（可能出现线程超额订阅）"
                );
                return;
            }
        };
        let _ = ort::init().with_global_thread_pool(opts).commit();
    });
}

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
    pub(crate) analyzer: Arc<OARStructure>,
}

impl OcrEngine {
    /// 按 `(tier, layout)` 取/建引擎。首次命中才下载+构建 ONNX 模型（缓存于 $OAR_HOME），
    /// 后续同 key 零重载——OFD 双 OCR 调用、库模式重复 convert 均复用同实例。
    pub fn build(tier: OcrTier, layout: OcrLayout) -> Result<Arc<OcrEngine>> {
        let key = EngineKey { tier, layout };
        let mut cache = CACHE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(e) = cache.get(&key) {
            return Ok(Arc::clone(e));
        }
        let mut t = crate::timing::StageTimer::new();
        let engine = Arc::new(OcrEngine {
            analyzer: Arc::new(build_analyzer(tier, layout)?),
        });
        t.stage("model-load");
        // 注意：build_analyzer 返回 OARStructure（predict_images 在其上），非 Builder。
        cache.insert(key, Arc::clone(&engine));
        Ok(engine)
    }

    /// 对一组页面图跑 OCR，返回每页 `StructureResult`（页序保序，契约由断言守恒）。
    /// `threads` 控制**页级并发**：多页切成 chunk 用 rayon 并行 `predict_images`，
    /// 共享 `&self.analyzer`（已证 `Sync`）。
    ///
    /// P7：`timings` 非空时按 chunk 记录 OCR 耗时（key = chunk 起始页 idx，
    /// 粒度 = chunk_size 页）。P3 流水线落地后改为单页 key（届时单页喂 OCR，
    /// per-page 自然可得）。chunk 粒度定位拖尾区间已足够。
    pub fn predict(
        &self,
        images: Vec<RgbImage>,
        threads: usize,
        timings: Option<&crate::timing::PageTimings>,
    ) -> Result<Vec<oar_ocr::domain::structure::StructureResult>> {
        if images.is_empty() {
            return Ok(Vec::new());
        }
        let n = images.len();
        let threads = threads.max(1);
        let chunk_size = n.div_ceil(threads);
        // chunk 起始页 idx（计时 key）：0, chunk_size, 2*chunk_size, ...
        let per_chunk: Vec<Vec<_>> = images
            .into_par_iter()
            .chunks(chunk_size)
            .enumerate()
            .map(|(ci, chunk)| {
                let start = Instant::now();
                let r = self.analyzer.predict_images(chunk);
                if let Some(t) = timings {
                    t.record(
                        ci * chunk_size,
                        crate::timing::PageStage::Ocr,
                        start.elapsed().as_secs_f64() * 1000.0,
                    );
                }
                r
            })
            .collect();

        let mut out = Vec::with_capacity(n);
        for group in per_chunk {
            for r in group {
                out.push(r.map_err(|e| runtime(None, format!("OCR 推理失败: {e}")))?);
            }
        }
        if out.len() != n {
            // 库模式不 panic 宿主：页序契约破坏改为显式 Err（CLI 会打印退出，库调用方可捕获）。
            return Err(runtime(
                None,
                format!("OCR 输出页数 {} != 输入 {}（页序契约破坏）", out.len(), n),
            ));
        }
        Ok(out)
    }

    /// 释放全部缓存的引擎（仅弃缓存自身 Arc 引用；外部仍持有的句柄继续有效）。
    /// 长驻服务需要周期性回收模型内存时调用。
    pub fn clear_cache() {
        let mut cache = CACHE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.clear();
    }
}

/// 模型名 → 加载路径。默认返回裸文件名，交给 oar-ocr 的 auto-download 走
/// `$OAR_HOME` 缓存（存在且 sha256 匹配则复用，否则从 ModelScope 下载）。
///
/// 设了 `ANYDOC_MODEL_DIR` 则拼成该目录下的**绝对路径**：oar-ocr 的
/// `download::resolve_path` 首条规则是"路径已存在即原样信任"，故可加载任意
/// 自备模型（如 INT8 量化版）而不触发注册表 hash 校验与重下载。
/// 目录内缺某个模型时回退裸名（该模型仍走正常下载通路）。
///
/// 注意：**不能**把自备模型放进 `$OAR_HOME` 再用裸名——那会命中
/// "parent 是缓存目录"分支，size/hash 不符即被静默重下载覆盖。
fn model_path(name: &str) -> String {
    match std::env::var("ANYDOC_MODEL_DIR") {
        Ok(dir) if !dir.is_empty() => {
            let p = std::path::Path::new(&dir).join(name);
            if p.is_file() {
                return p.to_string_lossy().into_owned();
            }
            name.to_string()
        }
        _ => name.to_string(),
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
    OARStructureBuilder::new(model_path(layout_model))
        .layout_model_name(layout_name)
        .with_ocr(
            model_path(spec.det),
            model_path(spec.rec),
            model_path(spec.dict),
        )
        // 表格结构识别（轻量：slanet_plus + 分类 + 字典）
        .with_table_classification(model_path(spec.table_cls))
        // 通用结构适配器：Wired/Wireless/Unknown 三分支无专用 adapter 时均回退到它，
        // 避免 table_cls 分类为 Wired 时因无 wired adapter 触发 config_error 整页失败
        .with_table_structure_recognition(model_path(spec.table_structure), "wireless")
        .table_structure_dict_path(model_path(spec.table_dict))
        // P1：文档方向矫正（0°/90°/180°/270°）——扫描件旋转/歪斜时 det/rec 召回关键。
        // 模型三档已定义（pp-lcnet_x1_0_doc_ori），此前未接入。在版面前自动矫正，
        // 改变 OCR 行为（旋转页结果变正），golden 需 UPDATE=1 重基线（预期召回提升）。
        .with_document_orientation(model_path(spec.doc_ori))
        .build()
        .map_err(|e| runtime(None, format!("构建 OCR 分析器失败: {e}")))
}

/// 全库唯一 OCR 入口（PDF/OFD 两通路共用）：用 oar-ocr 对渲染图做版面+文本+表格分析。
///
/// 等价 `OcrEngine::build(tier, layout)?.predict(images, threads, timings)`——模型按 `(tier, layout)`
/// 单例缓存，跨文档/跨调用复用，免重复加载。本函数兼负进程级 ORT 线程池初始化：须在
/// `OcrEngine::build` 创建 ONNX session 之前提交（Ticket A 生效点）。页级并行（rayon）、
/// 零拷贝消费、OCR 页序契约断言均在 `OcrEngine` 内统一实现。
///
/// P7：`timings` 非空时按 chunk 记录 OCR 耗时，调用方负责 `report()`。
pub fn ocr_images(
    images: Vec<RgbImage>,
    tier: OcrTier,
    layout: OcrLayout,
    threads: usize,
    timings: Option<&crate::timing::PageTimings>,
) -> Result<Vec<oar_ocr::domain::structure::StructureResult>> {
    // A：OCR 入口已知页级并行度，提交进程级线程池（消除超额订阅）。
    init_runtime(threads);
    OcrEngine::build(tier, layout)?.predict(images, threads, timings)
}
