//! 分阶段计时（仅当 ANYDOC_TIMINGS 环境变量存在时输出到 stderr）
//! 用于"速度优化"阶段的可观测性，不影响正常输出。
//!
//! P7：新增 per-page 计时收集器 [`PageTimings`]——render/OCR 每页耗时入收集器，
//! 结束时输出 per-page 明细 + p50/p95 直方图，定位拖尾页（异构 OCR 耗时）。
//! 与 [`StageTimer`]（文档级粗粒度阶段）互补。
use std::time::Instant;

pub struct StageTimer {
    start: Instant,
    last: Instant,
    enabled: bool,
}

impl StageTimer {
    pub fn new() -> Self {
        let now = Instant::now();
        StageTimer {
            start: now,
            last: now,
            enabled: std::env::var_os("ANYDOC_TIMINGS").is_some(),
        }
    }

    /// 记录一个阶段，打印「自上一阶段耗时 / 累计耗时」。
    pub fn stage(&mut self, name: &str) {
        if !self.enabled {
            return;
        }
        let now = Instant::now();
        let delta_ms = now.duration_since(self.last).as_secs_f64() * 1000.0;
        let total_ms = now.duration_since(self.start).as_secs_f64() * 1000.0;
        eprintln!(
            "[timing] {:<10} +{:7.1}ms  (total {:8.1}ms)",
            name, delta_ms, total_ms
        );
        self.last = now;
    }
}

/// per-page 耗时阶段（与 [`PageTimings`] 配合）。
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PageStage {
    /// 渲染（PDFium / ofd-core）
    Render,
    /// OCR 推理（含 layout/det/rec/table 各 stage）
    Ocr,
}

/// per-page 计时收集器：记录每页各 stage 耗时，结束时输出明细 + 直方图。
///
/// 深模块：调用方只需 `start(page, stage)` / `end(page, stage)`，直方图/排序/输出
/// 全在内部。删除测试——删掉后 per-page 耗时散到 PDF/OFD 两调用方各 eprintln，
/// 复杂度重现 → earning its keep。
///
/// 线程安全：`Mutex<HashMap>`，可跨 rayon OCR 线程记录（P3 流水线落地后需此）。
pub struct PageTimings {
    /// (page_idx, stage) → 累计耗时 ms（同页同 stage 多次记录累加）
    inner: std::sync::Mutex<std::collections::HashMap<(usize, PageStage), f64>>,
    enabled: bool,
}

impl PageTimings {
    pub fn new() -> Self {
        PageTimings {
            inner: std::sync::Mutex::new(std::collections::HashMap::new()),
            enabled: std::env::var_os("ANYDOC_TIMINGS").is_some(),
        }
    }

    /// 是否启用（调用方据此决定是否构造计时 guard，省非计时场景开销）。
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// 记录一页某 stage 的耗时（ms）。同页同 stage 多次记录累加。
    pub fn record(&self, page: usize, stage: PageStage, ms: f64) {
        if !self.enabled {
            return;
        }
        let mut m = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        *m.entry((page, stage)).or_insert(0.0) += ms;
    }

    /// 输出 per-page 明细 + p50/p95 直方图到 stderr。
    /// 在文档转换结束时调用。
    pub fn report(&self) {
        if !self.enabled {
            return;
        }
        let m = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if m.is_empty() {
            return;
        }
        // 收集每页 render/ocr 耗时
        let mut pages: std::collections::BTreeMap<usize, (f64, f64)> =
            std::collections::BTreeMap::new();
        for (&(p, s), &ms) in m.iter() {
            let e = pages.entry(p).or_insert((0.0, 0.0));
            match s {
                PageStage::Render => e.0 += ms,
                PageStage::Ocr => e.1 += ms,
            }
        }
        eprintln!("[timing] ── per-page (ms) ──");
        eprintln!("[timing] {:>6} {:>10} {:>10} {:>10}", "page", "render", "ocr", "total");
        let mut render_vals: Vec<f64> = Vec::new();
        let mut ocr_vals: Vec<f64> = Vec::new();
        let mut total_vals: Vec<f64> = Vec::new();
        for (p, (r, o)) in &pages {
            let t = r + o;
            eprintln!("[timing] {:>6} {:>10.1} {:>10.1} {:>10.1}", p, r, o, t);
            render_vals.push(*r);
            ocr_vals.push(*o);
            total_vals.push(t);
        }
        eprintln!("[timing] ── histogram ──");
        for (label, vals) in [("render", &render_vals), ("ocr", &ocr_vals), ("total", &total_vals)] {
            let (p50, p95, max) = percentiles(vals);
            eprintln!(
                "[timing] {:<6} p50={:>7.1}  p95={:>7.1}  max={:>7.1}  (n={})",
                label,
                p50,
                p95,
                max,
                vals.len()
            );
        }
    }
}

/// 排序后取 p50/p95/max。空集返回 0。
fn percentiles(vals: &[f64]) -> (f64, f64, f64) {
    if vals.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    let mut v = vals.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    let p50 = v[n / 2];
    let p95_idx = ((n as f64) * 0.95).ceil() as usize;
    let p95 = v[p95_idx.saturating_sub(1).min(n - 1)];
    let max = v[n - 1];
    (p50, p95, max)
}

impl Default for PageTimings {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_basic() {
        // 1..=10，p50=5/6 边界，p95≈10，max=10
        let v: Vec<f64> = (1..=10).map(|x| x as f64).collect();
        let (p50, p95, max) = percentiles(&v);
        assert!((p50 - 5.0).abs() < 1e-9 || (p50 - 6.0).abs() < 1e-9);
        assert!(p95 >= 9.0);
        assert_eq!(max, 10.0);
    }

    #[test]
    fn percentiles_empty() {
        assert_eq!(percentiles(&[]), (0.0, 0.0, 0.0));
    }

    #[test]
    fn percentiles_single() {
        assert_eq!(percentiles(&[42.0]), (42.0, 42.0, 42.0));
    }
}
