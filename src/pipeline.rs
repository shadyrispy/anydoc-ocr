//! P3：render↔OCR 双段流水线（ADR-0002）。
//!
//! 现状（P3 前）：PDF/OFD 两通路都是"先全量渲染物化所有页图 → 再批量 OCR"，
//! 渲染延迟完全不被掩盖，峰值内存 = N×页图（52p×100dpi≈1.4GB）。
//!
//! P3：渲染线程逐页产出图 → 有界 channel 背压送入 → 消费线程逐页 OCR → 按 idx 回填。
//! - 渲染延迟被 OCR 掩盖（N 页渲染时 N-1 页在 OCR）
//! - 峰值内存降到 ~2×页图（渲染中 + OCR 中，channel bound = threads×2）
//! - PDFium `PdfDocument` / OFD `OfdReader` 非 Send → 渲染在专属单线程（闭包内 open）
//!
//! P0-1b：OCR 消费从"rayon 池并发"收敛为**单消费者**——oar 的 `OrtInfer` 每模型
//! 恒单 session（`vec![Mutex<Session>]` ×1），页级并发只会在 session 锁上 convoy
//! 且实测高并发死锁（见 `ocr_engine::OcrEngine::infer_lock` 文档）。真并行来自：
//! 渲染线程与消费线程的重叠 + ORT intra-op 线程池在单次 run 内的 batch 并行。
//!
//! A1（`ANYDOC_ORT_SESSION_POOL>1`）：core 每模型建 N session 池后，convoy 前提
//! 消除，消费侧放开为 `threads` 个共享 channel 的消费者（`engine.concurrent_infer()`
//! 为真时）；默认（池=1）仍严格单消费者——golden 行为与串行时代逐字节一致。
//!
//! 深模块 [`PagePipeline`]：拥有 render 闭包 + 背压 + OCR 消费，暴露 `run()` 返回
//! 按页序的结果。删除测试——删掉后 channel/spawn/回填逻辑散到 PDF/OFD 两调用方，
//! 复杂度重现 → earning its keep。
//!
//! 设计：不抽 `RenderSource` trait（PdfDocument/OfdReader 非 Send，trait 难以 clean），
//! 改用 `RenderFn: FnOnce(mpsc::Sender) -> Result<()> + Send`——调用方传入打开 doc +
//! 逐页渲染的闭包，在 spawn 内执行。两调用方（PDF/OFD）各自构造闭包，共享 OCR 消费
//! 逻辑（本模块 `run` 内）。
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

use crate::error::{ConvertError, Result, Stage, runtime};
use crate::ocr_engine::OcrEngine;
use crate::timing::PageTimings;
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
pub trait RenderFn: FnOnce(mpsc::SyncSender<RenderItem>) -> Result<()> + Send + 'static {}
impl<F: FnOnce(mpsc::SyncSender<RenderItem>) -> Result<()> + Send + 'static> RenderFn for F {}

/// A1 多消费者共享的收集状态：按 idx 归位的三张表（消费者线程并发写入，
/// 全部 join 后由 `run` 取出）。单消费者路径同样经此结构（无锁竞争开销可忽略），
/// 保证两种模式的收集语义完全一致。
struct ConsumerState {
    results: std::sync::Mutex<
        std::collections::BTreeMap<(usize, usize), Result<oar_ocr::domain::structure::StructureResult>>,
    >,
    render_errors: std::sync::Mutex<std::collections::BTreeMap<(usize, usize), ConvertError>>,
    page_dims: std::sync::Mutex<std::collections::BTreeMap<(usize, usize), (u32, u32)>>,
}

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
    /// bound = threads×2：渲染最多领先 OCR 2 轮，控峰值内存（仅内存背压语义；
    /// OCR 线程并发由 run() 内专用线程池 `num_threads(threads)` 精确限定）。
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
        std::collections::BTreeMap<(usize, usize), (u32, u32)>,
    )> {
        let bound = self.threads * Self::BOUND_MULT;
        let (tx, rx) = mpsc::sync_channel(bound);

        // 渲染线程：执行 render_fn，逐页产出
        let render_fn = self.render_fn;
        let render_handle = thread::Builder::new()
            .name("anydoc-render".into())
            .spawn(move || render_fn(tx))
            .map_err(|e| runtime(Stage::Render, None, format!("启动渲染线程失败: {e}")))?;

        // OCR 消费：默认（session 池=1）为**单消费者**循环——oar 每模型单 session，
        // 页级并发只有锁 convoy（且有死锁实证，见 `ocr_engine::OcrEngine::infer_lock`
        // 文档），`predict_one` 内部已引擎级串行化。真并行 = 渲染线程（生产者）
        // 与本消费线程重叠 + ORT intra-op 在单次 run 内的 batch 并行。
        //
        // A1（池>1，`engine.concurrent_infer()`）：起 `threads` 个消费者共享同一
        // channel 接收端（`Receiver` 非 Clone → `Arc<Mutex<Receiver>>`：recv 阻塞
        // 在锁上排队取件，推理本体在锁外并发——与"每模型 N session 池"配合即
        // 真页级并行）。结果统一按 idx 收集，最终升序输出——每页推理与消费者
        // 数无关，输出与单消费者逐字节一致。
        //
        // 背压语义不变：channel bound = threads×BOUND_MULT，OCR 忙时渲染线程
        // 在 `send` 上阻塞，峰值内存 ~(2+消费者数)×页图。
        let dump_on = std::env::var("ANYDOC_DUMP_DIR")
            .ok()
            .filter(|s| !s.is_empty())
            .is_some();
        let consumers = if self.engine.concurrent_infer() { self.threads } else { 1 };
        let shared = Arc::new(ConsumerState {
            results: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            render_errors: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            page_dims: std::sync::Mutex::new(std::collections::BTreeMap::new()),
        });
        let rx = Arc::new(std::sync::Mutex::new(rx));
        let mut handles = Vec::with_capacity(consumers);
        for _ in 0..consumers {
            let rx = Arc::clone(&rx);
            let engine = Arc::clone(&self.engine);
            let state = Arc::clone(&shared);
            let timings = self.timings.clone();
            handles.push(
                thread::Builder::new()
                    .name("anydoc-ocr".into())
                    .spawn(move || loop {
                        // 取件：单消费者 fast path 直接 recv（无锁竞争）；多消费者
                        // 经 Mutex 排队。recv Err = 渲染端全部 drop → 收工。
                        let item = match rx.try_lock() {
                            Ok(g) => match g.recv() {
                                Ok(item) => item,
                                Err(_) => return,
                            },
                            // 别的消费者在取件：本次让位轮询（有界，见下）
                            Err(_) => {
                                std::thread::yield_now();
                                match rx.lock().unwrap().try_recv() {
                                    Ok(item) => item,
                                    Err(mpsc::TryRecvError::Empty) => continue,
                                    Err(mpsc::TryRecvError::Disconnected) => return,
                                }
                            }
                        };
                        let (idx, img) = match item {
                            Ok((idx, img)) => (idx, img),
                            Err((idx, e)) => {
                                state.render_errors.lock().unwrap().insert(idx, e);
                                continue;
                            }
                        };
                        if dump_on {
                            let dims = (img.width(), img.height());
                            state.page_dims.lock().unwrap().insert(idx, dims);
                        }
                        // P0-1：与批量路径共用同一推理入口（错误包装/计时契约一致）
                        let res = engine.predict_one(img, idx.0, idx.1, timings.as_deref());
                        state.results.lock().unwrap().insert(idx, res);
                    })
                    .map_err(|e| runtime(Stage::Ocr, None, format!("启动 OCR 线程失败: {e}")))?,
            );
        }
        // 消费者线程各持 Clone 的 Arc<Mutex<Receiver>>；主线程释放自己的两份
        // 引用，渲染线程 drop(tx) 后 recv 得 Disconnected，全体自然收尾。
        drop(rx);

        // 等渲染线程结束（先 join 渲染再 join 消费者：渲染 Err 也要让消费者收工）
        let render_result = render_handle.join();
        for h in handles {
            let _ = h.join();
        }
        match render_result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e), // render_fn 返回致命错误
            Err(e) => return Err(runtime(Stage::Render, None, format!("渲染线程 panic: {e:?}"))),
        }

        let ConsumerState { results, render_errors, page_dims } = Arc::try_unwrap(shared)
            .unwrap_or_else(|_| panic!("pipeline 状态共享计数异常（消费者线程未全部退出）"));
        let results = results.into_inner().unwrap_or_else(|p| p.into_inner());
        let render_errors = render_errors.into_inner().unwrap_or_else(|p| p.into_inner());
        let page_dims = page_dims.into_inner().unwrap_or_else(|p| p.into_inner());

        // 收集按 (doc_idx, page_idx) 升序结果
        let mut out = Vec::with_capacity(results.len());
        for (idx, res) in results {
            match res {
                Ok(r) => out.push((idx, r)),
                Err(e) => return Err(e),
            }
        }
        Ok((out, render_errors.into_iter().collect(), page_dims))
    }
}
