//! ADR-0007：基于 OCR 置信度的档位升级门控。
//!
//! 问题：单一默认档位（tiny/100）无法兼顾"清晰扫描件要快"与"污染扫描件要准"。
//! 早期方案（图像质量路由）：渲染前 N 页 → 算 Laplacian/噪声/对比度指标 → 阈值树
//! 分级 → 映射 (tier, dpi)。经 2026-08-16 架构审查（improve-codebase-architecture
//! 候选 1）改为后验方案：
//!
//! **后验置信度门控**：OCR 自身输出的文本区域置信度（`TextRegion::confidence`）是
//! 比图像质量卷积更直接、更可靠的质量信号——识别不可靠必然置信度低。因此：
//! - 移除图像质量卷积（Laplacian/Sobel/Gaussian 手写 ~70 行）与 PDF/OFD 两处探针；
//! - 改为 tiny 跑首页 OCR → 平均置信度低于阈值 → 升级 small 全篇重跑。
//!
//! 收益：清晰件零开销（不探针、不额外渲染）；天然支持批处理混合质量（每文档独立
//! 后验，不受"首文档代表整批"限制）；quality 模块缩为 ~60 行深而窄的纯函数，
//! 只依赖 `StructureResult`，无通路依赖（PDF/OFD 各自只负责提供首页探针图）。
use clap::ValueEnum;
use oar_ocr::domain::structure::StructureResult;

/// 质量路由开关。`Off` 时退回 `--ocr-tier` 显式值（golden 测试用）。
///
/// 注意：默认 **Off**——实测 tiny/100dpi 对清晰件已达标，路由只在显式开启时
/// 承担"污染件升级 small"的额外首页 OCR 开销。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum QualityRoute {
    /// 后验门控：tiny 跑首页，平均置信度低于阈值 → 升级 small 全篇重跑
    Auto,
    /// 关闭路由，用显式 `--ocr-tier`
    #[default]
    Off,
}

/// 升级阈值：首页 OCR 平均置信度低于此值视为"识别不可靠"，升级 higher tier 重跑。
/// PP-OCR 识别置信度通常 0.9+，清晰件均值远高于 0.6；污染/小字件均值显著下滑。
pub const CONFIDENCE_UPGRADE_THRESHOLD: f32 = 0.6;

/// 探针页文本区域平均置信度；无 `text_regions` 或置信度全缺 → `None`
/// （调用方保守处理：视为不可信）。
pub fn mean_confidence(page: &StructureResult) -> Option<f32> {
    let regs = page.text_regions.as_ref()?;
    let mut sum: f32 = 0.0;
    let mut n: usize = 0;
    for r in regs {
        if let Some(c) = r.confidence {
            sum += c;
            n += 1;
        }
    }
    (n > 0).then(|| sum / n as f32)
}

/// 首页 OCR 结果是否需要升级到 higher tier（ADR-0007 后验门控）。
///
/// 判定经 P1.6 信号目录：`fallback::probe_signals` 产出 `LowConfidenceProbe`
/// （均值低于阈值或不可读，保守升级）→ 升级。该信号只驱动 tier 升级，不参与
/// 文字层/OCR 路由（`fallback::decide` 显式忽略它）。
pub fn needs_upgrade(page: &StructureResult) -> bool {
    crate::fallback::probe_signals(page).contains(&crate::fallback::FallbackSignal::LowConfidenceProbe)
}

/// 页级"需要按页重试"判定（T2）。与 [`needs_upgrade`] 同一信号
/// （`fallback::probe_signals` 的 `LowConfidenceProbe`），但语义化为**单页**判定：
/// 该页 OCR 结果不可靠 → 用更高档局部重跑（而非全篇升级）。
///
/// 复用而非新逻辑：均值<阈值 / 无 text_regions / 置信度全缺均保守判"需重试"。
pub fn page_needs_retry(page: &StructureResult) -> bool {
    needs_upgrade(page)
}

#[cfg(test)]
mod tests {
    use super::*;
    use oar_ocr::domain::text_region::TextRegion;
    use oar_ocr::processors::{BoundingBox, Point};
    use std::sync::Arc;

    fn box11() -> BoundingBox {
        BoundingBox::new(vec![Point::new(0.0, 0.0), Point::new(10.0, 10.0)])
    }

    fn page_with(confs: &[Option<f32>]) -> StructureResult {
        let mut page = StructureResult::new("t", 0);
        let regs: Vec<TextRegion> = confs
            .iter()
            .map(|&c| TextRegion::with_recognition(box11(), Some(Arc::from("字")), c))
            .collect();
        page.text_regions = Some(regs);
        page
    }

    #[test]
    fn high_confidence_no_upgrade() {
        let page = page_with(&[Some(0.95), Some(0.9), Some(0.88)]);
        assert!(!needs_upgrade(&page), "高置信度不应升级");
    }

    #[test]
    fn low_confidence_upgrades() {
        let page = page_with(&[Some(0.3), Some(0.2), Some(0.4)]);
        assert!(needs_upgrade(&page), "低置信度应升级");
    }

    #[test]
    fn missing_text_regions_upgrades_conservatively() {
        let page = StructureResult::new("t", 0);
        assert!(needs_upgrade(&page), "无 text_regions 应保守升级");
    }

    #[test]
    fn all_confidence_none_upgrades_conservatively() {
        let page = page_with(&[None, None]);
        assert!(needs_upgrade(&page), "置信度全缺应保守升级");
    }

    #[test]
    fn mixed_mean_decides() {
        // 均值 (0.9+0.1)/2 = 0.5 < 0.6 → 升级
        let page = page_with(&[Some(0.9), Some(0.1)]);
        assert!(needs_upgrade(&page));
        // 均值 (0.9+0.7)/2 = 0.8 >= 0.6 → 不升级
        let page2 = page_with(&[Some(0.9), Some(0.7)]);
        assert!(!needs_upgrade(&page2));
    }

    #[test]
    fn page_needs_retry_mirrors_upgrade() {
        // 页级重试判定与升级判定同信号：低置信度页需重试，高置信度页不重试
        let bad = page_with(&[Some(0.2), Some(0.4)]);
        assert!(page_needs_retry(&bad));
        let good = page_with(&[Some(0.95), Some(0.92)]);
        assert!(!page_needs_retry(&good));
        // 无文本区域保守判重试
        assert!(page_needs_retry(&StructureResult::new("t", 0)));
    }
}
