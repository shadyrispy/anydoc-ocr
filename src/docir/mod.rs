//! P1.5 DocIR：版面级中间表示（spec ADR）。
//!
//! 现状（P1.5 前）：三源各自为政——PDF/OFD 文字层通路与 OCR（`gfm_adapter`）各自
//! 持 `BTreeMap<u32,String>` 段表 + `emitter` 跨页表挂起状态机，装配逻辑同构微差、
//! 跨页表合并藏在 emitter 可变状态里不可单测。
//!
//! P1.5 后：三源（PDF 文字层 / OFD 文字层 / OCR StructureResult）各自只**产**
//! [`DocIR`]（producer：完成来源特有的提取/排序/标题级别赋值，产出最终行/表格区块）；
//! IR 后处理 pass（[`passes::cross_page_table`]，纯函数、可单测）与渲染层
//! （[`render`]，只消费 DocIR，不依赖 pdf/ofd 内部类型或 StructureResult，AC-6）
//! 统一消费。跨页表合并从 emitter 状态机迁出为 pass（AC-7）。
//!
//! 页粒度的来源标注 [`PageSource`] 驱动渲染风格（正文行装配/表格 flush 格式的
//! 历史差异），混合文档（文字层页 + OCR 页）按页各自渲染，字节级行为与旧通路
//! 一致（golden 守护，AC-8）。
//!
//! Region 扩展见 [`crate::region`]（`kind` + `confidence`）。

pub mod content_list;
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

/// 页尺寸来源（#6 第 1 步）：决定该页的 bbox 能否安全归一化。
///
/// 归一化 bbox（MinerU 严格 middle_json 的 0–1 约定，`docvortex/schema.py:463-485`）
/// 的分母必须是**页面框**，用"内容外扩"当分母会让归一化值系统性偏大甚至 >1。
/// 故这里显式记录来源，渲染/投影层据此决定"能归一化"还是"只能给未归一化坐标"。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageDimsKind {
    /// 未知（无区块、或来源通路拿不到尺寸）——`w`/`h` 为 0。
    Unknown,
    /// **真实页面框**：OCR 路 = 送 OCR 的位图尺寸；OFD 路按页型取
    /// `PageObject.area.physical_box`（mm）或渲染位图（px）。
    PageBox,
    /// **内容外扩**（max x_max / max y_max）：PDF 文字层唯一拿得到的量
    /// （pdf-inspector 的 `CropBox ∩ MediaBox` 是 `pub(crate)`，见
    /// `extractor/mod.rs:14,125`，不 vendor 就拿不到）。
    /// → 该页 bbox **不可**归一化，只能输出原始 pt。
    ContentExtent,
    /// **真实页面框（pt）+ PDF y 语义**：#11b-v2。文字层页拿得到页框时记录
    /// （`pdf/text_layer.rs::page_visible_boxes`，lopdf 读 MediaBox/CropBox，
    /// 口径复刻 pdf-inspector `visible_page_box`：CropBox∩MediaBox 优先）。
    ///
    /// 与 [`PageBox`] 的差别在 **y 方向**：本仓 IR 全局约定 y 越小越靠上
    /// （top-down），但 PDF 文字层的行框是"baseline 翻转"形态——
    /// `y_min = -baseline`、`y_max = -baseline + em`（见
    /// `pdf/text_layer.rs::push_line_region`，**不是**纯 `-y` 翻转；改它会让
    /// 混排字号时 reading_order 排序漂移，零回归红线不许动）。
    /// 投影层换算 PDF 系框 `[baseline, baseline+em]`：
    /// `y_pdf = -y_min`，`h = y_max - y_min`；再转 top-down：
    /// `y0_top = H + 2·y_min - y_max`，`y1_bottom = H + y_min`。
    /// 单测 `bbox_pdf_pt_baseline_flip_roundtrip` 钉住公式。
    PageBoxPdfPt,
}

/// 页尺寸**单位**（#6 第 1 步）：与 `Region` 坐标的单位配对使用，二者必须一致
/// 才能做归一化。一个常量，不做派生换算——px↔mm 只在"整页光栅化"时成固定比例，
/// ADR-0008 直提分支下位图是内嵌 image object 的原生像素（`src/pdf/render.rs:192-201`），
/// 与页面物理尺寸不成固定比例，反算必错。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageUnit {
    /// 未知（伴随 `PageDimsKind::Unknown`）。
    Unknown,
    /// 像素：OCR 路（送推理的位图宽高）。
    Px,
    /// pt（PDF 用户单位）：PDF 文字层的 `ContentExtent`。
    Pt,
    /// 毫米：OFD `PhysicalBox`（OFD 页面坐标系即 mm，见 `ofd/text_layer.rs` 头注）。
    Mm,
}

/// 页尺寸：`(w, h)` + 来源（[`PageDimsKind`]）+ 单位（[`PageUnit`]）。
///
/// **每页记录的是"该页自己区块所在坐标空间"的分母**：
/// - PDF 文字层页 → 内容外扩 pt（`ContentExtent`，不可归一化）；
/// - OFD 文字层页 → 物理框 mm（`PageBox`）；
/// - OCR 页（PDF/OFD 皆然）→ 送 OCR 的位图 px（`PageBox`）。
/// 混合文档里两类页各自成立；OFD 的 OCR 页若渲染失败回落文字层，则按 mm 记录。
#[derive(Clone, Copy, Debug)]
// #6 第 1 步是**纯 IR 增量**：producer 写 dims，渲染层（第 2/4 步）与 middle_json
// 投影（#10）才开始读。此处的 dead_code 是刻意的"先落数据、零行为变化"，
// 消费方是第 2/4 步（渲染）与 #10（bbox 投影）；本步由下列单测与
// `gfm_adapter` 的 `dims_do_not_affect_rendered_markdown` 钉住"写了但不影响输出"。
#[allow(dead_code)]
pub struct PageDims {
    /// 页宽（单位见 `unit`）。
    pub w: f32,
    /// 页高。
    pub h: f32,
    /// 来源（能否归一化的依据）。
    pub kind: PageDimsKind,
    /// 单位。
    pub unit: PageUnit,
}

impl Default for PageDims {
    fn default() -> Self {
        Self { w: 0.0, h: 0.0, kind: PageDimsKind::Unknown, unit: PageUnit::Unknown }
    }
}

impl PageDims {
    /// 真实页面框（像素）：OCR 路。
    pub fn page_box_px(w: u32, h: u32) -> Self {
        Self { w: w as f32, h: h as f32, kind: PageDimsKind::PageBox, unit: PageUnit::Px }
    }
    /// 真实页面框（毫米）：OFD 文字层页的 `PhysicalBox`。
    pub fn page_box_mm(w: f32, h: f32) -> Self {
        Self { w, h, kind: PageDimsKind::PageBox, unit: PageUnit::Mm }
    }
    /// 内容外扩（pt）：PDF 文字层唯一拿得到的量。
    pub fn extent_pt(w: f32, h: f32) -> Self {
        Self { w, h, kind: PageDimsKind::ContentExtent, unit: PageUnit::Pt }
    }
    /// 真实页面框（pt，PDF y 语义 = baseline-flip，见
    /// [`PageDimsKind::PageBoxPdfPt`]）：PDF 文字层页（#11b-v2）。
    pub fn page_box_pdf_pt(w: f32, h: f32) -> Self {
        Self { w, h, kind: PageDimsKind::PageBoxPdfPt, unit: PageUnit::Pt }
    }
    /// 是否可用作归一化分母（`PageBox` 且两维 > 0）。
    #[allow(dead_code)] // 同上：消费方是 #10 的 bbox 投影。
    pub fn normalizable(&self) -> bool {
        matches!(self.kind, PageDimsKind::PageBox | PageDimsKind::PageBoxPdfPt)
            && self.w > 0.0
            && self.h > 0.0
    }
}

/// 版面级页 IR：一页的区块集合 + 来源 + 页尺寸（#6 第 1 步）。
#[derive(Clone, Debug)]
pub struct PageIR {
    /// 页号（文档内 0 基；跨文档场景由调用方保证唯一）。
    pub page_no: u32,
    /// 区块（正文行/网格表/表格 HTML/成品块，见 `RegionKind`）。
    pub regions: Vec<Region>,
    /// 来源标注。
    pub source: PageSource,
    /// 页尺寸（投影 bbox 归一化的分母；来源与单位见 [`PageDims`]）。
    ///
    /// **渲染层不消费此字段**——第 1 步是纯 IR 增量，markdown 输出逐字节不变
    /// （golden 守护，见 BACKLOG #6 验收判据"第 1 步单独零回归"）。
    pub dims: PageDims,
}

/// 输出格式（#11）：IR 是真相，格式只是**投影**。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OutputFormat {
    /// GFM markdown（默认，行为与 #11 前逐字节一致）。
    #[default]
    Markdown,
    /// content_list v2（`list[list[{type, content, bbox}]]` 的 JSON）。
    ContentListV2,
}

/// 文档级 IR：页序即输出序。
#[derive(Clone, Debug, Default)]
pub struct DocIR {
    pub pages: Vec<PageIR>,
}

/// 终渲染：跨页表 pass → 按格式投影。
///
/// `ocr_render` = OCR 源（`ANYDOC_EMIT_FURNITURE` 生效）；文字层源恒 `false`
/// ——两条通路历史上渲染风格就不同（见 [`PageSource`]），这里原样保留，
/// #11 不改 markdown 的一个字节。
pub(crate) fn finalize(doc: DocIR, fmt: OutputFormat, ocr_render: bool) -> String {
    finalize_with_opts(doc, fmt, ocr_render, render::RenderOpts::from_env())
}

/// [`finalize`] 的**选项显式**版本（见 [`render::RenderOpts`]）。
///
/// 存在的理由同上：`RenderOpts` 一旦只经env 读取，测试就无法固定档位——
/// `pdf::tests::merge_md` 断言的是「空页被丢弃后只剩两页段」，与页标记无关，
/// 不该被 #9 gap-A 的默认档带着变。
pub(crate) fn finalize_with_opts(
    doc: DocIR,
    fmt: OutputFormat,
    ocr_render: bool,
    opts: render::RenderOpts,
) -> String {
    let mut doc = doc;
    passes::cross_page_table::run(&mut doc);
    match fmt {
        OutputFormat::Markdown => {
            let emit = ocr_render && std::env::var("ANYDOC_EMIT_FURNITURE").is_ok();
            render::render_with_opts(&doc, emit, opts)
        }
        OutputFormat::ContentListV2 => content_list::to_content_list_v2_json(&doc),
    }
}

impl DocIR {
    /// 追加一页（页序即装配序）。`dims` 由 producer 给：拿不到就传
    /// [`PageDims::default`]（`Unknown`），**不要**伪造一个看起来合理的值——
    /// 下游投影靠 `kind` 判定能否归一化（见 [`PageDims`]）。
    pub fn push_page(
        &mut self,
        page_no: u32,
        source: PageSource,
        regions: Vec<Region>,
        dims: PageDims,
    ) {
        self.pages.push(PageIR {
            page_no,
            regions,
            source,
            dims,
        });
    }

    /// 渲染为 GFM 文本（按页分段、段间空行；详见 [`render`]）。
    ///
    /// 生产路径走 [`finalize`]（pass + 按格式投影），本方法留给单测直接看渲染结果。
    #[allow(dead_code)]
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
        doc.push_page(0, PageSource::TextLayerPdf, vec![], PageDims::extent_pt(600.0, 800.0));
        doc.push_page(1, PageSource::Ocr, vec![], PageDims::page_box_px(1240, 1754));
        assert_eq!(doc.pages.len(), 2);
        assert_eq!(doc.pages[0].page_no, 0);
        assert_eq!(doc.pages[1].source, PageSource::Ocr);
        // #6 第 1 步：dims 原样落页，kind 决定能否归一化。
        assert_eq!(doc.pages[0].dims.kind, PageDimsKind::ContentExtent);
        assert!(!doc.pages[0].dims.normalizable());
        assert!(doc.pages[1].dims.normalizable());
        assert_eq!(doc.pages[1].dims.unit, PageUnit::Px);
        assert_eq!((doc.pages[1].dims.w, doc.pages[1].dims.h), (1240.0, 1754.0));
    }

    /// 拿不到页面框的通路必须记 `Unknown`（0×0、不可归一化），而不是伪造一个值。
    #[test]
    fn page_dims_unknown_is_not_normalizable() {
        let d = PageDims::default();
        assert_eq!(d.kind, PageDimsKind::Unknown);
        assert_eq!(d.unit, PageUnit::Unknown);
        assert!(!d.normalizable());
        // 有尺寸但来源不是页面框（内容外扩）同样不可归一化。
        assert!(!PageDims::extent_pt(612.0, 792.0).normalizable());
        // 0 尺寸的 PageBox（异常数据）也不可。
        assert!(!PageDims::page_box_px(0, 0).normalizable());
        assert!(PageDims::page_box_mm(210.0, 297.0).normalizable());
    }
}
