//! P1.6：OCR 回退决策集中（spec ADR）。
//!
//! 现状（P1.6 前）：回退判定散落——PDF `text_layer_markdown` 5 处 `Ok(None)`
//! 内联路由（空层/浅检乱码/深检乱码/家具剔除后空/装配为空），OFD `convert_ofd`
//! 内联 `text_len < 5 && img_count > 0` 与 `is_garbled_text` 页型判定。规则改动
//! 须同时 hunt 两个通路，且无决策表单测。
//!
//! 集中后：三源通路只**供信号**（[`FallbackSignal`]），路由规则收敛为纯函数
//! [`decide`]——规则改动一处生效，决策表有单测锚定（AC-9/AC-10）。
//!
//! 粒度：PDF 为文档级（任一文档级信号 → 整文档 OCR），OFD 为页级（混合文档
//! 页级路由）。`Scope::Doc` → `Route::OcrDoc`，`Scope::Page` → `Route::OcrPage`。
//!
//! 注意：`--pdf-force-ocr`/`--ofd-force-ocr` 是 CLI 强制开关，**不属信号**，
//! 由调用方短路（spec P2：force_flags 维持独立）。置信度探针
//! （[`FallbackSignal::LowConfidenceProbe`]）只影响 tier 升级（`quality.rs` 消费），
//! 不改变文字层/OCR 路由——列入枚举是完整信号目录，决策表中显式断言其不触发路由。

/// 回退信号：三源通路检测到的"文字层不可信/不可用"证据。
///
/// 信号本身不做路由判断——是否构成回退由 [`decide`] 的决策表统一裁定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackSignal {
    /// 文字层为空：提取 0 项 / `[Image:` 占位符过滤后为空 / 家具剔除后无页 /
    /// 装配输出为空（PDF 文档级：扫描件）。
    EmptyTextLayer,
    /// 坏字体浅检：替换符/私有区/控制字符占比超标（PDF `looks_garbled`、
    /// OFD `is_garbled_text`，字符分类收敛于 `text_health`）。
    GarbledShallow,
    /// 坏字体深检：pdf-inspector 系统性乱码页占比达标（拉丁扩展乱码兜底，
    /// PDF 文档级专用）。
    GarbledDeep,
    /// 页面存在图片对象（OFD ImageObject；图文混排页也可能有，单独不构成回退）。
    ImageObjectPresent,
    /// 页文字字符数低于阈值（OFD `IMAGE_PAGE_MIN_TEXT_CHARS`；单独不构成回退——
    /// 空白页无图时文字层路径仍正确产出空结果）。
    BelowCharThreshold,
    /// 置信度探针：tiny 首页平均置信度低于升级阈值（ADR-0007）。只驱动 tier
    /// 升级，不驱动路由。
    LowConfidenceProbe,
}

/// 决策粒度：PDF 文档级 / OFD 页级。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// 页级（OFD）：单页回退，混合文档其余页仍走文字层。
    Page,
    /// 文档级（PDF）：任一文档级信号 → 整文档 OCR。
    Doc,
}

/// 路由结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// 文字层可用，正常提取输出。
    TextLayer,
    /// 该页回退 OCR（页级粒度）。
    OcrPage,
    /// 整文档回退 OCR（文档级粒度）。
    OcrDoc,
}

impl Route {
    /// 是否路由到 OCR（页/文档级通用的"回退了吗"谓词）。
    pub fn is_ocr(self) -> bool {
        matches!(self, Route::OcrPage | Route::OcrDoc)
    }
}

/// 回退决策表（纯函数）：信号 → 路由。
///
/// 规则（与 P1.6 前两通路行为逐一镜像，golden 守护）：
/// - [`FallbackSignal::EmptyTextLayer`] / [`GarbledShallow`](FallbackSignal::GarbledShallow) /
///   [`GarbledDeep`](FallbackSignal::GarbledDeep) 任一出现 → OCR（PDF 文档级三信号；
///   OFD 页级只用 GarbledShallow——页级"空层"以 BelowCharThreshold+ImageObjectPresent
///   组合表达，见下）。
/// - `BelowCharThreshold` **且** `ImageObjectPresent` 同时出现 → OCR（OFD 图片型页：
///   文字少 + 有图 = 扫描图页）。单独出现任一均不回退：有图有字是图文混排正常页；
///   无图少字是空白页，文字层路径产出空结果即可。
/// - `LowConfidenceProbe` 单独或与其他信号并存均不改变路由（tier 升级走 quality）。
/// - 无任何回退信号 → TextLayer。
pub fn decide(signals: &[FallbackSignal], scope: Scope) -> Route {
    let has = |s: FallbackSignal| signals.contains(&s);
    let ocr = has(FallbackSignal::EmptyTextLayer)
        || has(FallbackSignal::GarbledShallow)
        || has(FallbackSignal::GarbledDeep)
        || (has(FallbackSignal::BelowCharThreshold) && has(FallbackSignal::ImageObjectPresent));
    if !ocr {
        return Route::TextLayer;
    }
    match scope {
        Scope::Page => Route::OcrPage,
        Scope::Doc => Route::OcrDoc,
    }
}

/// 置信度探针信号提取（ADR-0007）：OCR 探针页文本区域平均置信度低于升级阈值
/// （或无 `text_regions` / 置信度全缺，保守视为不可信）→ `LowConfidenceProbe`。
///
/// 消费方是 `quality::needs_upgrade`（tier 升级），**不是** [`decide`]——质量
/// 维度与路由维度正交。阈值常量 `CONFIDENCE_UPGRADE_THRESHOLD` 归 quality
/// （质量域参数），信号目录归本模块（单一真相）。
pub fn probe_signals(page: &oar_ocr::domain::structure::StructureResult) -> Vec<FallbackSignal> {
    let low = crate::quality::mean_confidence(page)
        .is_none_or(|c| c < crate::quality::CONFIDENCE_UPGRADE_THRESHOLD);
    if low {
        vec![FallbackSignal::LowConfidenceProbe]
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ocr(signals: &[FallbackSignal], scope: Scope) -> bool {
        decide(signals, scope).is_ocr()
    }

    // ── 文档级（PDF）──

    /// 无信号 → 文字层。
    #[test]
    fn doc_no_signals_stays_text_layer() {
        assert_eq!(decide(&[], Scope::Doc), Route::TextLayer);
    }

    /// 空文字层（扫描件）→ 整文档 OCR。
    #[test]
    fn doc_empty_text_layer_routes_ocr_doc() {
        assert_eq!(decide(&[FallbackSignal::EmptyTextLayer], Scope::Doc), Route::OcrDoc);
    }

    /// 浅检乱码 → 整文档 OCR。
    #[test]
    fn doc_shallow_garbled_routes_ocr_doc() {
        assert_eq!(decide(&[FallbackSignal::GarbledShallow], Scope::Doc), Route::OcrDoc);
    }

    /// 深检乱码 → 整文档 OCR。
    #[test]
    fn doc_deep_garbled_routes_ocr_doc() {
        assert_eq!(decide(&[FallbackSignal::GarbledDeep], Scope::Doc), Route::OcrDoc);
    }

    /// 文档级三信号全命中 → 仍 OcrDoc（幂等，不叠加）。
    #[test]
    fn doc_all_garbled_signals_route_ocr_doc() {
        let sigs = [
            FallbackSignal::EmptyTextLayer,
            FallbackSignal::GarbledShallow,
            FallbackSignal::GarbledDeep,
        ];
        assert_eq!(decide(&sigs, Scope::Doc), Route::OcrDoc);
    }

    // ── 页级（OFD）──

    /// 图片型页：少字 + 有图 → OcrPage（OFD `text_len < 5 && img_count > 0` 镜像）。
    #[test]
    fn page_low_text_with_image_routes_ocr_page() {
        let sigs = [FallbackSignal::BelowCharThreshold, FallbackSignal::ImageObjectPresent];
        assert_eq!(decide(&sigs, Scope::Page), Route::OcrPage);
    }

    /// 少字但无图（空白页）→ 文字层（产出空结果，不浪费 OCR）。
    #[test]
    fn page_low_text_without_image_stays_text_layer() {
        assert_eq!(
            decide(&[FallbackSignal::BelowCharThreshold], Scope::Page),
            Route::TextLayer
        );
    }

    /// 有图但文字充足（图文混排正常页）→ 文字层。
    #[test]
    fn page_image_with_enough_text_stays_text_layer() {
        assert_eq!(
            decide(&[FallbackSignal::ImageObjectPresent], Scope::Page),
            Route::TextLayer
        );
    }

    /// 页级浅检乱码 → OcrPage（OFD F3 坏字体页）。
    #[test]
    fn page_garbled_routes_ocr_page() {
        assert_eq!(decide(&[FallbackSignal::GarbledShallow], Scope::Page), Route::OcrPage);
    }

    /// 页级 EmptyTextLayer 语义上只由 PDF 文档级发出；若误用于页级也判回退
    /// （防御：信号目录单一真相，宁回退不漏检）。
    #[test]
    fn page_empty_text_layer_also_routes_ocr() {
        assert!(ocr(&[FallbackSignal::EmptyTextLayer], Scope::Page));
    }

    // ── 边界与不参与路由的信号 ──

    /// 置信度探针单独出现 → 不改变路由（tier 升级由 quality.rs 消费）。
    #[test]
    fn low_confidence_probe_alone_does_not_route() {
        for scope in [Scope::Page, Scope::Doc] {
            assert_eq!(decide(&[FallbackSignal::LowConfidenceProbe], scope), Route::TextLayer);
        }
    }

    /// 置信度探针叠加其他信号 → 不放大也不抑制回退。
    #[test]
    fn low_confidence_probe_does_not_change_existing_route() {
        let sigs = [FallbackSignal::LowConfidenceProbe, FallbackSignal::GarbledShallow];
        assert_eq!(decide(&sigs, Scope::Page), Route::OcrPage);
        let sigs = [FallbackSignal::LowConfidenceProbe, FallbackSignal::ImageObjectPresent];
        assert_eq!(decide(&sigs, Scope::Page), Route::TextLayer);
    }

    /// 深检信号只在 PDF 文档级产生；页级收到也判回退（防御性一致）。
    #[test]
    fn page_deep_garbled_routes_ocr_page() {
        assert_eq!(decide(&[FallbackSignal::GarbledDeep], Scope::Page), Route::OcrPage);
    }

    /// is_ocr 谓词：TextLayer 恒 false，OcrPage/OcrDoc 恒 true。
    #[test]
    fn is_ocr_predicate_matches_route() {
        assert!(!Route::TextLayer.is_ocr());
        assert!(Route::OcrPage.is_ocr());
        assert!(Route::OcrDoc.is_ocr());
    }
}
