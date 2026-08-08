//! 分阶段计时（仅当 ANYDOC_TIMINGS 环境变量存在时输出到 stderr）
//! 用于"速度优化"阶段的可观测性，不影响正常输出。
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
