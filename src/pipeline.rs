//! P3：render↔OCR 双段流水线（ADR-0002）。
//!
//! 现状（P3 前）：PDF/OFD 两通路都是"先全量渲染物化所有页图 → 再批量 OCR"，
//! 渲染延迟完全不被掩盖，峰值内存 = N×页图（52p×100dpi≈1.4GB）。
//!
//! P3：渲染线程逐页产出图 → 有界 channel 背压送入 → rayon 池并发 OCR → 按 idx 回填。
//! - 渲染延迟被 OCR 掩盖（N 页渲染时 N-1 页在 OCR）
//! - 峰值内存降到 ~2×页图（渲染中 + OCR 中，channel bound = threads×2）
//! - PDFium `PdfDocument` / OFD `OfdReader` 非 Send → 渲染在专属单线程（闭包内 open），
//!   OCR 用 rayon 池
//!
//! 深模块 [`PagePipeline`]：拥有 render 闭包 + OCR 池 + 背压，暴露 `run()` 返回按页序
//! 的结果。删除测试——删掉后 channel/spawn/回填逻辑散到 PDF/OFD 两调用方，
//! 复杂度重现 → earning its keep。
//!
//! 设计：不抽 `RenderSource` trait（PdfDocument/OfdReader 非 Send，trait 难以 clean），
//! 改用 `RenderFn: FnOnce(mpsc::Sender) -> Result<()> + Send`——调用方传入打开 doc +
//! 逐页渲染的闭包，在 spawn 内执行。两调用方（PDF/OFD）各自构造闭包，共享 OCR 消费
//! 逻辑（本模块 `run` 内）。
//!
//! P4 预加载折叠：detect 判定走 OCR 后，render 线程 spawn 前 `thread::spawn` 后台
//! `OcrEngine::build`，与渲染并行。build 失败 → run() 返回 Err（原本也是）。
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;

use crate::error::{ConvertError, Result, runtime};
use crate::ocr_engine::OcrEngine;
use crate::timing::{PageStage, PageTimings};
use image::RgbImage;

/// 渲染项：((doc_idx, page_idx), 渲染结果)。渲染失败时 idx 仍须已知（调用方按页容错）。
/// 跨文档流水线（ADR-0005 候选 2）：doc_idx 区分文档，page_idx 为文档内页号。
/// 单文档调用方传 doc_idx=0。
///
/// ADR-0006：错误类型从 `anyhow::Error` 升级为 `ConvertError`，渲染失败归
/// `Malformed { part: "page N", detail }`（运行时错误，非文档本身问题）。
pub(crate) type RenderItem =
    std::result::Result<((usize, usize), RgbImage), ((usize, usize), ConvertError)>;

/// 渲染器闭包：在专属线程内 open doc + 逐页渲染，产出 (idx, img) 入 channel。
/// 闭包返回 Ok(()) 表示渲染完毕，Err 表示致命错误（doc 打开失败等）。
///
/// 约束 `Send`：闭包捕获 path 等 Send 数据，在 spawn 内执行（doc 在闭包内 open，
/// 不跨线程）。
pub trait RenderFn:
    FnOnce(mpsc::SyncSender<RenderItem>) -> Result<()> + Send + 'static
{
}
impl<F: FnOnce(mpsc::SyncSender<RenderItem>) -> Result<()> + Send + 'static> RenderFn for F {}

/// render↔OCR 流水线：渲染线程 + rayon OCR 池 + 有界背压。
///
/// 用法：`PagePipeline::new(render_fn, engine, threads, timings).run()`
/// 返回 `Result<Vec<StructureResult>>`（按页序 0..n，n 由 render_fn 产出页数决定）。
///
/// page_count 不预知（避免调用方为取 count 而提前 open doc）：run 内用 BTreeMap
/// 按 idx 动态收集，最终排序输出。渲染失败页（idx 已知但 OCR 缺失）跳过告警。
pub struct PagePipeline<F: RenderFn> {
    render_fn: F,
    engine: Arc<OcrEngine>,
    threads: usize,
    timings: Option<Arc<PageTimings>>,
}

impl<F: RenderFn> PagePipeline<F> {
    /// bound = threads×2：渲染最多领先 OCR 2 轮，控峰值内存。
    const BOUND_MULT: usize = 2;

    pub fn new(
        render_fn: F,
        engine: Arc<OcrEngine>,
        threads: usize,
        timings: Option<Arc<PageTimings>>,
    ) -> Self {
        PagePipeline {
            render_fn,
            engine,
            threads: threads.max(1),
            timings,
        }
    }

    /// 启动流水线，返回按页序的 (idx, result)。
    ///
    /// 渲染线程执行 `render_fn`，逐页产出图入有界 channel；
    /// rayon scope 并发消费 channel，每页一个任务，结果按 idx 回填到 BTreeMap。
    ///
    /// 返回 `Vec<(idx, StructureResult)>` 按 idx 升序——保留 idx 以便调用方处理
    /// 渲染失败页（失败页 idx 缺失，调用方按 idx 容错）。
    ///
    /// 错误传播：
    /// - 渲染失败页 → 该 idx 缺失（调用方容错）
    /// - OCR 失败页 → run() 返回 Err（调用方按页回退文字层/报错）
    /// - 渲染线程 panic/致命错误 → channel 关闭，run() 返回 Err
    ///
    /// 返回 `(成功页, 渲染错误列表)`——渲染失败（单页或整文档）不再被静默丢弃，
    /// 调用方可回填结构化错误（ADR 候选 3：错误 detail 走 Result 通道而非 stderr）。
    pub fn run(
        self,
    ) -> Result<(
        Vec<((usize, usize), oar_ocr::domain::structure::StructureResult)>,
        Vec<((usize, usize), ConvertError)>,
    )> {
        let bound = self.threads * Self::BOUND_MULT;
        let (tx, rx) = mpsc::sync_channel(bound);

        // 渲染线程：执行 render_fn，逐页产出
        let render_fn = self.render_fn;
        let render_handle = thread::Builder::new()
            .name("anydoc-render".into())
            .spawn(move || render_fn(tx))
            .map_err(|e| runtime(None, format!("启动渲染线程失败: {e}")))?;

        // OCR 消费：rayon scope 并发，按 (doc_idx, page_idx) 回填到 BTreeMap
        let results: std::sync::Mutex<
            std::collections::BTreeMap<(usize, usize), Result<oar_ocr::domain::structure::StructureResult>>,
        > = std::sync::Mutex::new(std::collections::BTreeMap::new());
        // 渲染失败（单页 / 整文档）的错误，按 idx 收集供调用方回填。
        let render_errors: std::sync::Mutex<
            std::collections::BTreeMap<(usize, usize), ConvertError>,
        > = std::sync::Mutex::new(std::collections::BTreeMap::new());
        let results_ref = &results;
        let render_errors_ref = &render_errors;
        let engine_ref = &self.engine;
        let timings_ref = self.timings.as_deref();

        rayon::scope(|s| {
            let rx = rx;
            while let Ok(item) = rx.recv() {
                let (idx, img) = match item {
                    Ok((idx, img)) => (idx, img),
                    Err((idx, e)) => {
                        render_errors_ref.lock().unwrap().insert(idx, e);
                        continue;
                    }
                };
                let engine = engine_ref;
                let timings = timings_ref;
                s.spawn(move |_| {
                    let start = std::time::Instant::now();
                    let r = engine.analyzer.predict_images(vec![img]);
                    if let Some(t) = timings {
                        t.record(idx.1, PageStage::Ocr, start.elapsed().as_secs_f64() * 1000.0);
                    }
                    let res = match r.into_iter().next() {
                        Some(Ok(s)) => Ok(s),
                        Some(Err(e)) => Err(runtime(
                            Some(&format!("page {:?}", idx)),
                            format!("OCR 推理失败: {e}"),
                        )),
                        None => Err(runtime(
                            Some(&format!("page {:?}", idx)),
                            "OCR 返回空结果".to_string(),
                        )),
                    };
                    results_ref.lock().unwrap().insert(idx, res);
                });
            }
        });

        // 等渲染线程结束
        match render_handle.join() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e), // render_fn 返回致命错误
            Err(e) => return Err(runtime(None, format!("渲染线程 panic: {e:?}"))),
        }

        // 收集按 (doc_idx, page_idx) 升序结果
        let map = results.into_inner().unwrap();
        let mut out = Vec::with_capacity(map.len());
        for (idx, res) in map {
            match res {
                Ok(r) => out.push((idx, r)),
                Err(e) => return Err(e),
            }
        }
        let errs = render_errors.into_inner().unwrap().into_iter().collect();
        Ok((out, errs))
    }
}
