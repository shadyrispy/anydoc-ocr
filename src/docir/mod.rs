//! P1.5 DocIR：版面级中间表示（spec ADR）。
//!
//! 现状（P1.5 前）：三源各自为政——PDF/OFD 文字层通路与 OCR（`gfm_adapter`）各自
//! 持 `BTreeMap<u32,String>` 段表 + `emitter` 跨页表挂起状态机，装配逻辑同构微差、
//! 跨页表合并藏在 emitter 可变状态里不可单测。
//!
//! P1.5 后：三源（PDF 文字层 / OFD 文字层 / OCR StructureResult）各自只**产**
//! [`DocIR`]（producer：完成来源特有的提取/排序/标题前缀，产出最终行/表格区块）；
//! IR 后处理 pass（[`passes::cross_page_table`]，纯函数、可单测）与渲染层
//! （[`render`]，只消费 DocIR，不依赖 pdf/ofd 内部类型或 StructureResult，AC-6）
//! 统一消费。跨页表合并从 emitter 状态机迁出为 pass（AC-7）。
//!
//! 页粒度的来源标注 [`PageSource`] 驱动渲染风格（正文行装配/表格 flush 格式的
//! 历史差异），混合文档（文字层页 + OCR 页）按页各自渲染，字节级行为与旧通路
//! 一致（golden 守护，AC-8）。
//!
//! Region 扩展见 [`crate::region`]（`kind` + `confidence`）。

pub mod passes;
pub(crate) mod render;

use crate::region::Region;

/// 页来源：三源统一标注（渲染风格分流的唯一依据）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageSource {
    /// PDF 文字层（pdf-inspector 提取）。
    TextLayerPdf,
    /// OFD 文字层（ofd-core 提取）。
    TextLayerOfd,
    /// OCR（渲染 + oar-ocr 推理）。
    Ocr,
}

/// 版面级页 IR：一页的区块集合 + 来源。
#[derive(Clone, Debug)]
pub struct PageIR {
    /// 页号（文档内 0 基；跨文档场景由调用方保证唯一）。
    pub page_no: u32,
    /// 区块（正文行/网格表/表格 HTML/成品块，见 `RegionKind`）。
    pub regions: Vec<Region>,
    /// 来源标注。
    pub source: PageSource,
}

/// 文档级 IR：页序即输出序。
#[derive(Clone, Debug, Default)]
pub struct DocIR {
    pub pages: Vec<PageIR>,
}

impl DocIR {
    /// 追加一页（页序即装配序）。
    pub fn push_page(&mut self, page_no: u32, source: PageSource, regions: Vec<Region>) {
        self.pages.push(PageIR {
            page_no,
            regions,
            source,
        });
    }

    /// 渲染为 GFM 文本（按页分段、段间空行；详见 [`render`]）。
    pub fn render(&self) -> String {
        render::render(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_page_appends_in_order() {
        let mut doc = DocIR::default();
        doc.push_page(0, PageSource::TextLayerPdf, vec![]);
        doc.push_page(1, PageSource::Ocr, vec![]);
        assert_eq!(doc.pages.len(), 2);
        assert_eq!(doc.pages[0].page_no, 0);
        assert_eq!(doc.pages[1].source, PageSource::Ocr);
    }
}
