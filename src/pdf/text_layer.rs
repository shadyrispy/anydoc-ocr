//! PDF 文字层提取管线：pdf-inspector 提取 TextItem → 列感知拆行 → 阅读顺序还原
//! → 组装 Markdown。含坏字体检测（浅检/深检两级）、跨页"页面家具/水印"剔除、
//! 表格候选启发式 + 末页探针 + 版面 OCR 确认、文字层网格表重建等。
//! OCR 通路（`ocr_engine`）与渲染（`render`）留在 `mod.rs`。

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use super::page_box::{PageBox, page_visible_boxes};
use crate::docir::{DocIR, PageDims, PageSource};
use crate::fallback::{self, FallbackSignal};
use crate::region::{Region, RegionKind, Span};
use crate::table_grid::{self};
use crate::{ConvertRequest, Result, gfm_adapter, reading_order};

/// garbled 检测常量：最多扫描前 4000 个 TextItem；字符总数须 >50，且
/// 坏字符占比 >=20%（bad * 100 >= total * 20）才判定为乱码
/// （阈值常量集中定义于 `text_health::GARBLED_MIN_TOTAL_CHARS` /
/// `GARBLED_BAD_PERCENT_THRESHOLD`，PDF/OFD 共用）。
const GARBLED_MAX_ITEMS: usize = 4000;

/// 文字层探针结论（anydoc 0.2.4 "Scanned pages are reported, not dropped"）。
pub(crate) enum TextHit {
    /// 文字层可用且**无缺页**：`DocIR`（跨页表 pass **前**），由调用方按
    /// [`crate::docir::OutputFormat`] 终渲染——markdown 与旧快速路径字节一致
    /// （golden 守护）。#11 前这里直接带 finalized 的 markdown 字符串，无法投影。
    Complete(DocIR),
    /// 混合文档：文字层只覆盖部分页。`text` = **跨页表 pass 前**的文字层 DocIR，
    /// `missing_pages` = 需要 OCR 的页（1 基，升序）。
    /// 调用方只渲/识别缺页，再按页号合并渲染（见 `pdf::merge_hybrid`）。
    Hybrid { text: DocIR, missing_pages: Vec<u32> },
}

/// 宽间隙阈值（页宽比例）：行内相邻 item 的 gap > 1% 页宽视为列间隙。
/// 三处共用同一口径——双列拆行的 x 段分裂、列判定（`clustered_row_split`）、
/// 表格候选启发式（`page_has_tabular_rows`，行被拆成 >=3 段判为疑似表格）。
const MIN_GAP_FRACTION: f32 = 0.01;

/// 列间隙聚类容差（页宽比例）：候选 gap 中点彼此相差 <=2% 页宽归为同一簇；
/// 主簇 >=3 行才确认为全局列间隙（双列页 gutter 每行同一 x，聚成主簇；
/// 封面/标题的字母间距是单行现象，聚不到 3 行 → 不拆）。
const SPLIT_CLUSTER_TOL_FRACTION: f32 = 0.02;

/// 列间隙最小宽度（页宽比例）：候选 gap 须 >=3% 页宽才算列间隙。
/// 与 `reading_order::detect_column_split` 的 3% 口径一致。1% 的 `MIN_GAP_FRACTION`
/// 会把列表项（`a) `、`b) ` 等编号与正文的小间隙 ~1%）误判为列间隙，单栏页
/// 聚出假 gutter（9001c 文字版 4.1/4.2 正文缺行根因）。
const COL_GUTTER_GAP_FRACTION: f32 = 0.03;

/// P1.6：本通路所有"文字层不可用"出口的统一走查——信号供集中决策表
/// [`fallback::decide`] 裁决（PDF 粒度=文档级）。判回退（`OcrDoc`）→ `Ok(None)`，
/// 调用方整文档转 OCR；判文字层（当前决策表对这些信号恒回退，见 fallback.rs
/// 决策表单测）→ 本出口已无正文可产，返回空 `Some("")` 而非半途残稿。
fn no_text_layer(signal: FallbackSignal) -> Result<Option<String>> {
    if fallback::decide(std::slice::from_ref(&signal), fallback::Scope::Doc).is_ocr() {
        return Ok(None);
    }
    Ok(Some(String::new()))
}

/// 轻量元数据（classify，不渲图，~10–50ms）：文档总页数。
/// 供 route_pdf 两处消费——页数闸（对齐 MinerU `max_pages_per_file=1000`，
/// `ANYDOC_MAX_PAGES` 可调）与 `--pages` 求值（rN/裁剪需要总页数）。
/// 错误经 `from_pdf_error` 分类（加密/损坏）；探针内部会重新触达同一错误源，
/// 分类语义不变。
pub(crate) fn classify_pages(path: &Path) -> crate::error::Result<u32> {
    let bytes = open_pdf_bytes(path).ok_or_else(|| {
        crate::error::ConvertError::io(
            crate::error::Stage::Extract,
            std::io::Error::other(format!("打开 PDF 失败: {}", path.display())),
        )
    })?;
    pdf_inspector::classify_pdf_mem(bytes.as_slice())
        .map(|c| c.page_count)
        .map_err(crate::error::from_pdf_error)
}

/// 文字层探针（P1.8 拆阶段 + anydoc 0.2.4 缺页上报）。
///
/// 返回 `None` = 整文档无可用文字层（扫描件/提取失败/坏字体），调用方整篇转 OCR
/// （与旧 `text_layer_markdown` 的 `Ok(None)` 完全一致）。
///
/// 返回 `Some(Complete)` = 旧快速路径，字节一致（golden 守护）。
/// 返回 `Some(Hybrid)` = 混合文档：文字层只覆盖部分页，`missing_pages`（1 基）
/// 即旧通路会**静默丢掉**的扫描页，交由调用方按页补 OCR 后合并（不再丢页）。
///
/// `text_only`（#13 `--text-only`）：本探针内**唯一**会加载模型的动作是
/// [`confirm_table_pages`]（表格候选页的版面 OCR 确认），此处直接跳过——不渲染、
/// 不建引擎，与 `--ocr-tier` 无关地保证"零模型加载"。其余判定（乱码/空层/缺页）
/// 照常上抛给调用方裁决（route_pdf 在 text_only 下把"该走 OCR"改为显式报错/告警）。
///
/// `select` = `--pages` 求值后的 1 基页集合（`None` = 全页，行为与历史逐字节
/// 一致）：抽取先行裁剪到所选页，缺页判定只在所选页内取交集，未选页不抽取、
/// 不渲染、不进输出。
///
/// 缺页判据（两段式，与 anydoc 0.2.4 同思路但用自家文字层复核）：
/// inspector 深检报 `pages_needing_ocr`（倾向多报：实测 `multipage.pdf` 8/8 页
/// 全标 scanned，而自家文字层能完整产正文）→ 仅当**本页在文字层 DocIR 里
/// 没有任何非空区块**时才算真缺页。这样纯文字文档恒为 Complete。
pub(crate) fn text_layer_probe(
    path: &Path,
    opts: &ConvertRequest,
    select: Option<&BTreeSet<u32>>,
    text_only: bool,
) -> Result<Option<TextHit>> {
    let (mut items, rotations) = extract_text_items(path)?;
    // --pages：抽取后先裁剪（后续浅检/家具/短路/装配全部只在所选页内工作）。
    if let Some(sel) = select {
        items.retain(|i| sel.contains(&i.page));
    }
    // 图片型/扫描件（过滤后 items 空）直接回落 OCR，跳过开销大的 garbled 预检。
    if items.is_empty() {
        // P1.6：空文字层信号（图片型/扫描件）→ 集中决策表裁决（文档级）。
        return no_text_layer(FallbackSignal::EmptyTextLayer).map(empty_route);
    }
    let mut hit = LayerHit { select: select.cloned(), ..Default::default() };
    if is_garbled_doc(path, &items, &mut hit) {
        return Ok(None);
    }
    let by_page = strip_furniture(items);
    // 扫描件防护：文字层仅有页眉/页码等零星重复文本时，家具过滤可能删光全部 →
    // by_page 为空 → 无可用文字层，回落 OCR（而非 panic）。
    if by_page.is_empty() {
        // P1.6：家具剔除后空层信号 → 集中决策表裁决（文档级）。
        return no_text_layer(FallbackSignal::EmptyTextLayer).map(empty_route);
    }
    // 审计 #8 附项（MinerU `MAX_NATIVE_TEXT_CHARS_PER_PAGE` 同语义）：单页原生
    // 字符超上限（默认 65535）→ 放弃该页文字层抽取，改记入"必 OCR"集合。
    // 动机同为防卡死：拆行 / 列间隙聚类 / 阅读序 / 表格启发式的耗时全部随整页
    // items 规模放大；宁可多付该页一次 OCR，也不产被拖慢或劣化的文字层页。
    // `ANYDOC_NO_HYBRID` 下不短路：该开关语义是"回到旧行为（缺页丢弃、纯文字
    // 层通路）"，此处旧行为 = 照旧抽取，宁慢不丢。
    // 全部页都超限时 by_page 空 → 走整文档 OCR（与逐页短路的净效果一致）。
    let by_page = if hybrid_disabled() {
        by_page
    } else {
        drop_oversized_pages(by_page, crate::limits::native_text_char_cap(), &mut hit)
    };
    if by_page.is_empty() {
        return no_text_layer(FallbackSignal::EmptyTextLayer).map(empty_route);
    }
    let (lines_by_page, page_w, page_h) = build_line_groups(&by_page);
    // P0-2：不 unwrap——防御式回落（无文字层 → 整文档 OCR）而非 panic。
    // P1.6：回退裁决走集中决策表（空层信号，文档级）。
    let last_page = match by_page.keys().next_back() {
        Some(&p) => p,
        None => return no_text_layer(FallbackSignal::EmptyTextLayer).map(empty_route),
    };
    let table_out = confirm_table_pages(path, opts, &by_page, &lines_by_page, &page_w, text_only);
    let last_table_md = probe_last_page_table(path, last_page, &page_w, &page_h);
    // #11b-v2：文字层页框（MediaBox/CropBox 继承解析）。open_pdf_bytes 走 mmap
    // 载体；读不出（空 map）→ 全页回落 ContentExtent，与 v2 之前一致。
    let page_boxes = open_pdf_bytes(path)
        .map(|b| page_visible_boxes(b.as_slice()))
        .unwrap_or_default();
    // pass 前的文字层 DocIR（混合时与 OCR 页合并后再统一跑 pass，见 pdf::merge_hybrid）
    let text = build_text_docir(
        &by_page,
        &lines_by_page,
        &page_w,
        &page_h,
        &table_out,
        last_page,
        last_table_md.as_deref(),
        &page_boxes,
        &rotations,
    );
    if render_of(&text).is_empty() {
        // P1.6：装配输出为空（空层信号，文档级）→ 集中决策表裁决。
        no_text_layer(FallbackSignal::EmptyTextLayer).map(empty_route)
    } else {
        let missing = hit.missing_pages(&text);
        if missing.is_empty() || hybrid_disabled() {
            Ok(Some(TextHit::Complete(text)))
        } else {
            Ok(Some(TextHit::Hybrid { text, missing_pages: missing }))
        }
    }
}

/// `ANYDOC_NO_HYBRID=1`：关闭按页混合路由，回到旧行为（缺页静默丢弃、整篇走
/// 文字层快速路径）。留作 A/B 与故障排查开关。
fn hybrid_disabled() -> bool {
    hybrid_disabled_from(std::env::var("ANYDOC_NO_HYBRID").ok().as_deref())
}

/// 开关语义（纯函数，可单测）：变量**存在即关闭**（不限值），未设置默认开启。
fn hybrid_disabled_from(v: Option<&str>) -> bool {
    v.is_some()
}

// `ANYDOC_RICH_TEXT` 已废弃（#6 决策 (c)）：它曾把 PDF 文字层的行内样式
// （bold/italic/underline/strikeout）注入成 `**粗**`/`*斜*`/`<u>`/`<s>` 字面量
// （借鉴 MinerU 4.0 `prepare/apply_text_evidence`），由这里读取、再把 `rich: bool`
// 一路透传给 `build_text_docir` → `build_oriented_page` → `oriented_group_regions`
// → `build_body_regions` → `push_line_region`。样式属于**结构**，不该以 Markdown
// 字面量的形式焊进正文再靠正则剥回来（当时为此造的判定视图 `text_health::
// strip_inline_style_markers` 也一并删除，见该模块注释），故本变量**不再改变
// 任何行为**，`rich` 参数链整体移除；
// 替代方案是结构化 span（`Region.spans`，#6 第 4 步），届时也不留渲染开关。
// 废弃告警在调度层统一打一次：见 [`crate::convert::warn_deprecated_env`]——
// 挂在本模块的文字层分支里会让"OFD / 纯扫描件 + 该变量"静默无提示。

/// 只渲染本页（不跑 pass）——用于"文字层是否产出了内容"的空判定。
fn render_of(doc: &DocIR) -> String {
    crate::docir::render::render(doc)
}

/// `no_text_layer` 出口 → [`TextHit`]：决策表判回退（`None`）时整文档 OCR；
/// 判保留文字层时（当前 PDF 文档级决策恒回退，此为防御分支）产退化空文档。
fn empty_route(o: Option<String>) -> Option<TextHit> {
    // #11：`Complete` 改带 DocIR 后，这里的"退化空文档"就是一个**空 IR**
    // （渲染为空串，与旧 `Complete("")` 逐字节一致）。旧签名带的是最终 markdown
    // 字符串，无法投影。
    o.map(|_| TextHit::Complete(DocIR::default()))
}

/// 深检副产物：inspector 全文档视角的总页数与"需 OCR"页集合（1 基）。
#[derive(Default)]
pub(crate) struct LayerHit {
    page_count: u32,
    needs_ocr: BTreeSet<u32>,
    /// 审计 #8 附项：字符超限被放弃文字层抽取的页（1 基）。这些页文字层 DocIR
    /// 里已无区块，无需 inspector 报 needs_ocr 也必须进缺页集合（直接并入）。
    forced_ocr: BTreeSet<u32>,
    /// `--pages` 选页集合（1 基，`None` = 全页）：缺页判定只在所选页内取交集
    /// ——未选页既不出现在文字层 DocIR（被裁剪），也绝不允许被当成缺页送去
    /// OCR（否则 `missing_pages` 校验永远补不齐，整文档 Err）。
    select: Option<BTreeSet<u32>>,
}

/// 单页字符短路（审计 #8 附项）：把整页原生字符数 > `cap` 的页从 `by_page`
/// 摘除并记入 [`LayerHit::forced_ocr`]，返回剩余页。纯函数（cap 显式传入、
/// `hit` 唯一可变副作用），可单测。
fn drop_oversized_pages(
    mut by_page: BTreeMap<u32, Vec<pdf_inspector::TextItem>>,
    cap: usize,
    hit: &mut LayerHit,
) -> BTreeMap<u32, Vec<pdf_inspector::TextItem>> {
    let oversized: Vec<u32> = by_page
        .iter()
        .filter(|(_, items)| {
            items.iter().map(|i| i.text.chars().count()).sum::<usize>() > cap
        })
        .map(|(&p, _)| p)
        .collect();
    for p in oversized {
        by_page.remove(&p);
        hit.forced_ocr.insert(p);
    }
    by_page
}

impl LayerHit {
    /// 缺页 = （inspector 报 needs_ocr ∩ 文字层 DocIR 中无非空内容的页 ∩ 选页）
    /// ∪ 字符短路强制页（1 基升序）。
    ///
    /// 复核基准是**文字层实际产出**（`text` 里该页号是否有非空段），而不是
    /// inspector 的抽取长度——多报的页（如 `multipage.pdf` 全部 8 页）在文字层
    /// 有正文即不算缺页，纯文字文档因此恒空 → Complete → 快速路径行为不变。
    /// `forced_ocr` 页不享受该复核：文字层已被主动放弃，必然要 OCR。
    /// `select`（`--pages`）：判定域收缩到所选页，未选页永不进缺页集合。
    fn missing_pages(&self, text: &DocIR) -> Vec<u32> {
        let in_select = |p: &u32| self.select.as_ref().is_none_or(|s| s.contains(p));
        let forced: BTreeSet<u32> = self.forced_ocr.iter().copied().filter(&in_select).collect();
        if (self.needs_ocr.is_empty() && self.forced_ocr.is_empty()) || self.page_count == 0 {
            return forced.into_iter().collect();
        }
        let mut covered: BTreeSet<u32> = BTreeSet::new();
        for page in &text.pages {
            if !page_is_empty(page) {
                covered.insert(page.page_no);
            }
        }
        let scan: BTreeSet<u32> = (1..=self.page_count)
            .filter(|p| self.needs_ocr.contains(p) && !covered.contains(p) && in_select(p))
            .collect();
        scan.union(&forced).copied().collect()
    }
}

/// 本页文字层是否没有任何非空区块。
/// `Grid` 无条件视为有内容（网格表 text 恒为空串，内容在结构体里）；
/// 其余 kind（含 `PreRendered`/`TableHtml`）按 `text.trim()` 判定——成品块
/// producer 只对有内容的页落块，trim 判空与"视为有内容"在实际数据上等价，
/// 但判空更保守（异常空块不会挡住 OCR 兜底）。
fn page_is_empty(page: &crate::docir::PageIR) -> bool {
    page.regions.iter().all(|r| match &r.kind {
        RegionKind::Grid(_) => false,
        _ => r.text.trim().is_empty(),
    })
}

/// 文本提取：pdf-inspector 全文 TextItem + 过滤图片占位符。
///
/// ADR-0006：错误不再吞 `Ok(None)`——加密 PDF（Encrypted）、损坏 PDF
/// （InvalidStructure/Parse/NotAPdf）按 `PdfError` 分类返 `Err`，
/// batch 预分流阶段据此直接标错、不送 OCR（避免绕一大圈丢失分类）。
/// Io 错误（文件读不到等）同样返 Err，由调用方处理。
///
/// #11b-v2：改走 `..._and_rotations_mem`（同一提取管线、同一坐标帧
/// `PositionFrame::Sheet`，逐 item 一致），副产物拿到每页坐标帧
/// `PageRotation`（"pages absent from the map are upright"）——供 dims 决策
/// 判定"该页坐标是否在可见框原点上"。mem 版无 `validate_pdf_file`，文件不可
/// 读由 `open_pdf_bytes` 的 None 分支补上（同 `classify_pages` 的 Io 分类）。
fn extract_text_items(
    path: &Path,
) -> Result<(Vec<pdf_inspector::TextItem>, HashMap<u32, pdf_inspector::PageRotation>)> {
    let Some(bytes) = open_pdf_bytes(path) else {
        return Err(crate::error::ConvertError::io(
            crate::error::Stage::Extract,
            std::io::Error::other(format!("打开 PDF 失败: {}", path.display())),
        ));
    };
    let (items, rotations) =
        pdf_inspector::extract_text_with_positions_and_rotations_mem(bytes.as_slice())
            .map_err(crate::error::from_pdf_error)?;
    // pdf-inspector 1.14+ 对图片对象返回 `[Image: ...]` 占位 TextItem（FormXob 引用等），
    // 非真实文字。过滤后判空——纯图片型 PDF（image.pdf/image_table.pdf）过滤后为空，
    // 回退 OCR，避免误判"有文字层"输出占位符。
    Ok((
        items
            .into_iter()
            .filter(|i| !i.text.trim_start().starts_with("[Image:"))
            .collect(),
        rotations,
    ))
}

/// 坏字体（GID/编码损坏）两级防护（T12）：浅检在前、深检兜底。
///
/// - 浅检（廉价）：前 4000 个 TextItem 中替换符/私有区/控制符占比 >=20% → 乱码
///   （字符分类见 `looks_garbled`）。命中即返回，省下深检的 ~0.3s 全页 markdown
///   构建（健康文档白付的成本）。拉丁扩展乱码（如某些 GID 字体）此处检不出，
///   由深检兜住。
/// - 深检（兜底）：pdf-inspector 健壮检测器全文档抽取，统计被判
///   `suspected_garbled_text` 的页数，占比 >=20% 且至少 3 页 → 系统性坏字体。
///   健康文档即使有少量误报（如目录点线符的私有区字符，上海公报仅 2 页）也不触发。
///   注意：pdf-inspector 内部行分组有 bug（layout.rs:1270 空列集 then_some 立即
///   求值 panic），extract_pages_markdown 也会触发——catch_unwind 兜底，panic 视为
///   "无法预检"跳过。T11：path 版内部 `fs::read` 整读（500MB 文档重复 500MB 堆
///   峰值），用 mmap 版 `_mem` 零拷贝映射，与末页探针同思路。
///
/// 两级信号均经 [`fallback::decide`]（文档级）集中裁决。
///
/// 副产物：深检那次全文档抽取的 `page_count` / `pages_needing_ocr` 回填进
/// `hit`，供 [`text_layer_probe`] 的缺页判定复用——**不再额外付一次全量抽取**
/// （anydoc 0.2.4 需两段式，我们的文字层通路本就跑过同一文档，复核基准换成
/// 自家 DocIR 的实际产出）。
fn is_garbled_doc(path: &Path, items: &[pdf_inspector::TextItem], hit: &mut LayerHit) -> bool {
    // 浅检
    if looks_garbled(items)
        && fallback::decide(&[FallbackSignal::GarbledShallow], fallback::Scope::Doc).is_ocr()
    {
        return true;
    }
    // 深检
    let Some(pdf_bytes) = open_pdf_bytes(path) else {
        return false; // 文件不可读 → 无法预检，继续文字层
    };
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        pdf_inspector::extract_pages_markdown_mem(pdf_bytes.as_slice(), None)
    }));
    let Ok(Ok(extraction)) = caught else {
        return false; // panic/提取失败 → 无法预检，继续文字层
    };
    let total = extraction.pages.len();
    hit.page_count = total as u32;
    hit.needs_ocr = extraction.pages_needing_ocr.iter().copied().collect();
    let garbled = extraction
        .ocr_reasons_by_page
        .iter()
        .filter(|r| {
            r.reasons
                .iter()
                .any(|s| s == pdf_inspector::OCR_REASON_SUSPECTED_GARBLED_TEXT)
        })
        .count();
    // 乱码页占比 >=20% 且至少 3 页 → 判定系统性坏字体，回退 OCR。
    garbled >= 3
        && total > 0
        && garbled * 100 >= total * 20
        && fallback::decide(&[FallbackSignal::GarbledDeep], fallback::Scope::Doc).is_ocr()
}

/// 跨页重复"页面家具/水印"剔除并按页分组（`TextItem.page` 1 起始，页序升序）。
///
/// 同文本 + 同归一化位置（x 中心、y 各 1% 箱）出现在 >=pages_needed 个不同页 →
/// 页眉/页脚/水印，剔除后再做行分组。单页/页数不足时 pages_needed > total_pages →
/// 零剔除（零误杀）。
fn strip_furniture(items: Vec<pdf_inspector::TextItem>) -> BTreeMap<u32, Vec<pdf_inspector::TextItem>> {
    let total_pages = items.iter().map(|i| i.page).max().unwrap_or(0) as usize;
    let pages_needed = std::cmp::max(3usize, ((total_pages as f32) * 0.6).ceil() as usize);
    let furniture = is_repeated_furniture(&items, pages_needed, total_pages);
    let items: Vec<pdf_inspector::TextItem> = items
        .into_iter()
        .filter(|i| !furniture.contains(&(i.page, i.x.to_bits(), i.y.to_bits(), i.text.clone())))
        .collect();
    let mut by_page: BTreeMap<u32, Vec<pdf_inspector::TextItem>> = BTreeMap::new();
    for item in items {
        by_page.entry(item.page).or_default().push(item);
    }
    by_page
}

/// 每页行组预构建（列检测与表格启发式共用一次）+ 每页近似宽/高缓存。
///
/// 空页防护：pdf-inspector 的 group_into_lines 对空 items 会 panic
/// （layout.rs index out of bounds）。某页无提取文本（如扫描件夹杂页）时跳过
/// 行分组，该页后续按无行处理（不崩、回落/空输出）。
/// pdf-inspector 自身 bug 兜底：group_into_lines 内部 `(len==2).then_some(columns[0])`
/// 对空列集立即求值 → index panic（layout.rs:1270）。正常页不触发、零开销；
/// 异常页回落空行组。
fn build_line_groups(
    by_page: &BTreeMap<u32, Vec<pdf_inspector::TextItem>>,
) -> (
    BTreeMap<u32, Vec<pdf_inspector::extractor::TextLine>>,
    BTreeMap<u32, f32>,
    BTreeMap<u32, f32>,
) {
    let mut lines_by_page: BTreeMap<u32, Vec<pdf_inspector::extractor::TextLine>> = BTreeMap::new();
    let mut page_w: BTreeMap<u32, f32> = BTreeMap::new();
    let mut page_h: BTreeMap<u32, f32> = BTreeMap::new();
    for (&page, page_items) in by_page {
        let mut w = 0.0_f32;
        let mut h = 0.0_f32;
        for i in page_items {
            w = w.max(i.x + i.width);
            h = h.max(i.y + i.height);
        }
        page_w.insert(page, w);
        page_h.insert(page, h);
        let lines = if page_items.is_empty() {
            Vec::new()
        } else {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                pdf_inspector::extractor::group_into_lines_preserving_all_text(page_items.clone())
            }))
            .unwrap_or_default()
        };
        lines_by_page.insert(page, lines);
    }
    (lines_by_page, page_w, page_h)
}

/// T2-B/R1：可疑表格页确认（单一信号，最终确认靠版面 OCR）。
///
/// 信号（唯一来源）：文字层启发式——某页 >=3 行各自被宽间隙拆成 >=3 个 x
/// 分离段。保守：双列正文每行仅 2 段（1 条 gutter），不会误报；真表格/目录行
/// 多为多列。有证据才渲染，避免为"可能有表"的猜测付整页版面 OCR。
/// 末页表格不走本集合：由 `probe_last_page_table` 独立兜底（pdf-inspector 表格
/// 提取，无渲染开销）；首页不强制入集（Ticket B：无证据召回，代价是每文档
/// 1~2 次整页渲染 + 版面 OCR）。
///
/// 懒渲染（仅可疑集非空才做）+ 批量版面 OCR（用 `opts.ocr.layout`，默认 Doc：
/// 含 table 类，能识别封面/版权栏等；Table 版面只标 Table，漏检严重）。确认
/// 有 `LayoutElementType::Table` 的页 → 单页 gfm（行 + `<table>`）；未确认页 →
/// 回落文字层（防误报，R2 gfm 过滤仍生效）。渲染/OCR 任一环节失败 → 该页
/// 回落，不炸文档。
fn confirm_table_pages(
    path: &Path,
    opts: &ConvertRequest,
    by_page: &BTreeMap<u32, Vec<pdf_inspector::TextItem>>,
    lines_by_page: &BTreeMap<u32, Vec<pdf_inspector::extractor::TextLine>>,
    page_w: &BTreeMap<u32, f32>,
    text_only: bool,
) -> BTreeMap<u32, String> {
    // #13：`--text-only` 是全库"零模型加载"契约的守点——这里若放行，文字型 PDF
    // 里一张疑似表页就会把整套 mineru 模型拉起来（首跑还要联网）。跳过即可：
    // 未确认的页本就回落文字层（下方 has_table 语义），输出只是少了 `<table>`
    // 结构、不缺内容。
    if text_only {
        return BTreeMap::new();
    }
    let mut suspicious: BTreeSet<u32> = BTreeSet::new();
    for (&page, lines) in lines_by_page {
        let Some(&w) = page_w.get(&page) else { continue };
        if page_has_tabular_rows(lines, w) {
            suspicious.insert(page);
        }
    }
    let mut table_out: BTreeMap<u32, String> = BTreeMap::new();
    if suspicious.is_empty() {
        return table_out;
    }
    // T07 懒惰渲染：只渲 `suspicious` 子集（0 基准 pdfium 页号 = p-1），
    // 避免 52p 文档全量渲 52 页只 OCR 3 页的内存浪费。`suspicious` 是
    // BTreeSet 升序，to_render 与 ocr_pages 同迭代序/同谓词 → 输出锁步。
    let to_render: Vec<u32> = suspicious
        .iter()
        .filter(|&&p| by_page.contains_key(&p))
        .map(|&p| p - 1)
        .collect();
    let ocr_pages: Vec<u32> = suspicious
        .iter()
        .filter(|&&p| by_page.contains_key(&p))
        .copied()
        .collect();
    if !to_render.is_empty()
        && let Ok(imgs) = super::render::render_pdf_pages(path, opts.render.dpi, &to_render)
        && !imgs.is_empty()
        && let Ok(results) = crate::ocr_engine::ocr_images(
            imgs,
            opts.ocr.tier,
            opts.ocr.layout,
            opts.parallel.page_parallel,
            None,
        )
    {
        for (page, res) in ocr_pages.into_iter().zip(results) {
            let has_table = res
                .layout_elements
                .iter()
                .any(|e| e.element_type == oar_ocr::domain::structure::LayoutElementType::Table);
            if has_table {
                // dims 传空：这里的产物是**文字层页**里的一个 PreRendered 块，该页
                // `PageIR.dims` 由 `build_text_docir` 按文字层坐标空间（pt 内容外扩）
                // 定死；把探针位图的 px 塞进来会造成同页两种单位混用（#6 第 1 步
                // 的单位口径），且渲染层第 1 步根本不消费 dims。
                table_out.insert(
                    page,
                    gfm_adapter::to_markdown(std::slice::from_ref(&res), &[]),
                );
            }
        }
    }
    table_out
}

/// 输出装配（P1.5 DocIR producer）：文字层网格表（免 OCR、跨页合并）+
/// OCR 确认表 + 普通行，页序混排。**返回 pass 前的 DocIR**——混合路由
/// （anydoc 0.2.4 缺页上报）要把 OCR 页按页号并进同一文档后再统一跑
/// `cross_page_table` pass；纯文字文档由 [`finalize_text_hit`] 走 pass +
/// 渲染，与旧通路字节一致（golden 守护）。
fn build_text_docir(
    by_page: &BTreeMap<u32, Vec<pdf_inspector::TextItem>>,
    lines_by_page: &BTreeMap<u32, Vec<pdf_inspector::extractor::TextLine>>,
    page_w_map: &BTreeMap<u32, f32>,
    page_h_map: &BTreeMap<u32, f32>,
    table_out: &BTreeMap<u32, String>,
    last_page: u32,
    last_table_md: Option<&str>,
    page_boxes: &BTreeMap<u32, PageBox>,
    rotations: &HashMap<u32, pdf_inspector::PageRotation>,
) -> DocIR {
    let mut doc = DocIR::default();
    for (page, page_items) in by_page.iter() {
        let (Some(&page_w), Some(full_lines)) = (page_w_map.get(page), lines_by_page.get(page))
        else {
            continue; // 不变量：两表均由 build_line_groups 从 by_page 构建，键恒一致
        };
        // #6 第 1 步 / #11b-v2：文字层页的"页尺寸"分两条来源——
        // - **可见页框**（`page_visible_boxes`，lopdf 复刻 pdf-inspector
        //   `CropBox ∩ MediaBox` 口径）：item 坐标系（visible box 帧，原点=框
        //   左下、y 向上）与框同帧 → 可作归一化分母，kind=`PageBoxPdfPt`
        //   （y 语义 = baseline-flip，换算见 `PageDimsKind::PageBoxPdfPt`）。
        //   前提是该页坐标帧 Upright（rotation map 无此页）。
        // - **整页内容流转正页**（rotation map 命中，Ccw/Cw）：item 已被
        //   pdf-inspector 转进 turned 帧，其 y 语义（转正帧的"页顶"方向、
        //   框-原点关系）与 baseline-flip 换算前提不兼容，且现网无实测样本
        //   验证对调宽高后的正确性 → 宁缺勿造，维持 `ContentExtent`
        //   （不可归一化、不给 bbox），缺口记 BACKLOG。
        // - **无框页**（页树无 MediaBox/CropBox）：维持 #6 第 1 步的
        //   `ContentExtent`（内容外扩），不可归一化。
        let dims = match (page_boxes.get(page), rotations.get(page)) {
            (Some(b), None) if b.width() > 0.0 && b.height() > 0.0 => {
                PageDims::page_box_pdf_pt(b.width(), b.height())
            }
            _ => PageDims::extent_pt(page_w, page_h_map.get(page).copied().unwrap_or(0.0)),
        };

        // 0) 朝向分组（借鉴 MinerU 表格朝向投票的常量族，见 `orientation` 模块）。
        // 整页单一朝向（`groups.len() == 1`，即全仓现网文档）→ 完全跳过本分支，
        // 下面 1)/2)/3) 的历史路径逐字节不变（golden 守护）。
        //
        // 多朝向才进来：旧路径把整页 items 一次性喂给网格重建，正立正文与
        // 旋转表块混成同一次聚类 → 表被**转置**（行列互换）、正文被吞进格子，
        // 或整页只剩正文丢掉表。分组后逐组独立走"网格表 → 正文行"，非正立组
        // 先旋回正立帧再重建。版面 OCR 已确认的表格页（步骤 2）优先级更高，
        // 故该页已在 `table_out` 时不进本分支。
        if !table_out.contains_key(page) {
            let groups = crate::orientation::vote_groups(
                &page_items.iter().map(|i| i.rotation).collect::<Vec<f32>>(),
            );
            if groups.len() > 1 {
                let mut out = build_oriented_page(page_items, &groups, *page, page_w);
                if *page == last_page && let Some(tbl) = last_table_md {
                    out.push(
                        Region::new(0.0, 0.0, 0.0, 0.0, format!("\n{tbl}\n"))
                            .with_kind(RegionKind::PreRendered),
                    );
                }
                doc.push_page(*page, PageSource::TextLayerPdf, out, dims);
                continue;
            }
        }

        // 1) 文字层网格表格（快速、免 OCR）：Grid 区块（同列续接/换列定格由 pass 承担）
        let blocks: Vec<Region> = page_items
            .iter()
            .map(|i| Region::from_top_left(i.x, -i.y, i.width, i.height, i.text.clone()))
            .collect();
        if let Some(grid) = table_grid::reconstruct_table_grid(&blocks, page_w) {
            doc.push_page(
                *page,
                PageSource::TextLayerPdf,
                vec![Region::new(0.0, 0.0, 0.0, 0.0, String::new())
                    .with_kind(RegionKind::Grid(grid))],
                dims,
            );
            continue;
        }

        // 2) 版面 OCR 确认的表格页：成品块（行 + <table>，producer 已含精确格式）
        if let Some(ocr_md) = table_out.get(page) {
            doc.push_page(
                *page,
                PageSource::TextLayerPdf,
                vec![Region::new(0.0, 0.0, 0.0, 0.0, ocr_md.clone())
                    .with_kind(RegionKind::PreRendered)],
                dims,
            );
            continue;
        }

        // 3) 普通页：文字层行（Body 区块）+ 末页表格探针兜底（成品块）
        let mut out = build_body_regions(full_lines, *page, page_w);
        // R3 兜底：末页布局未确认但 pdf-inspector 探针提取到表格（版权栏等小表格）
        // → 文字层行后追加管道表，保证表格信息不丢（保留正文行，仅追加结构）。
        if *page == last_page
            && let Some(tbl) = last_table_md
        {
            out.push(
                Region::new(0.0, 0.0, 0.0, 0.0, format!("\n{tbl}\n"))
                    .with_kind(RegionKind::PreRendered),
            );
        }
        doc.push_page(*page, PageSource::TextLayerPdf, out, dims);
    }
    doc
}

/// 多朝向页的装配（`orientation::vote_groups` 命中 >=2 组时才调用）。
///
/// 逐组独立走「网格表 → 正文行」。输出顺序：0° 组先出（页面自身叙事优先），
/// 其余按票数降序跟随——单页多朝向本就没有可靠的跨朝向阅读顺序，稳定可复现
/// 比猜测更值钱。
fn build_oriented_page(
    page_items: &[pdf_inspector::TextItem],
    groups: &[(u16, Vec<usize>)],
    page: u32,
    page_w: f32,
) -> Vec<Region> {
    let mut out = Vec::new();
    // groups 已按票数降序；这里只把 0° 组提到最前，其余保持票数降序。
    for take_upright in [true, false] {
        for &(angle, _) in groups {
            if (angle == 0) != take_upright {
                continue;
            }
            out.extend(oriented_group_regions(page_items, groups, angle, page, page_w));
        }
    }
    out
}

/// 单组（指定朝向）的区块构建：转正 → 试网格表 → 否则正文行。
///
/// 0° 组：不转不平移，`page_w` 用整页宽（与历史整页路径同口径）。
/// 非 0° 组：绕原点旋回正立（[`crate::orientation::to_upright_box`]）后平移到
/// 原点——平移不改相对几何，但让组内坐标与 `page_w` 估计回到历史口径。
fn oriented_group_regions(
    page_items: &[pdf_inspector::TextItem],
    groups: &[(u16, Vec<usize>)],
    angle: u16,
    page: u32,
    page_w_full: f32,
) -> Vec<Region> {
    let ids = match groups.iter().find(|(a, _)| *a == angle) {
        Some((_, ids)) => ids,
        None => return Vec::new(),
    };
    let mut up: Vec<pdf_inspector::TextItem> = Vec::with_capacity(ids.len());
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    for &i in ids {
        let src = &page_items[i];
        let (x, y, w, h) = crate::orientation::to_upright_box(src.x, src.y, src.width, src.height, angle);
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        let mut item = src.clone();
        item.x = x;
        item.y = y;
        item.width = w;
        item.height = h;
        item.rotation = 0.0; // 已转正，组内行分组按正立处理
        up.push(item);
    }
    if up.is_empty() {
        return Vec::new();
    }
    let page_w = if angle == 0 {
        page_w_full
    } else {
        if !min_x.is_finite() || !min_y.is_finite() {
            return Vec::new();
        }
        for item in &mut up {
            item.x -= min_x;
            item.y -= min_y;
        }
        up.iter()
            .map(|i| i.x + i.width)
            .fold(0.0_f32, f32::max)
            .max(1.0)
    };

    // 1) 该组是否自成一张网格表（列对齐 + >=2 行同列数）——表格优先，免 OCR
    let blocks: Vec<Region> = up
        .iter()
        .map(|i| Region::from_top_left(i.x, -i.y, i.width, i.height, i.text.clone()))
        .collect();
    if let Some(grid) = table_grid::reconstruct_table_grid(&blocks, page_w) {
        return vec![Region::new(0.0, 0.0, 0.0, 0.0, String::new())
            .with_kind(RegionKind::Grid(grid))];
    }

    // 2) 正文行：组内重新行分组。整页的 TextLine 是按整页 y 聚出来的，对旋转组
    // 无意义（跨朝向混行），故每组自成一次 group_into_lines。上游对畸形页有
    // 已知 panic（见 build_line_groups 注释），同样 catch_unwind 兜底。
    let lines = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        pdf_inspector::extractor::group_into_lines_preserving_all_text(up.clone())
    }))
    .unwrap_or_default();
    build_body_regions(&lines, page, page_w)
}

/// 普通页正文行构建：列间隙检测 + 双列拆行 → 阅读顺序 → 赋标题级别 → Body 区块。
fn build_body_regions(
    full_lines: &[pdf_inspector::extractor::TextLine],
    page: u32,
    page_w: f32,
) -> Vec<Region> {
    // 列间隙检测：行级候选间隙聚类。封面/标题的字母间距是单行现象、每行
    // split_x 各不相同，聚类不到 >=3 行；双列正文的 gutter 在每行同一 x 处
    // 重复出现，聚成主簇 → 只拆这些行，标题行保持整行。
    let split = clustered_row_split(full_lines, page_w);

    let mut regions: Vec<Region> = Vec::new();
    for line in full_lines {
        let mut sorted = line.items.clone();
        sorted.sort_by(|a, b| a.x.total_cmp(&b.x));
        // 找最接近全局 split 的内部间隙（若存在且够宽），从那里拆成左右两段
        let mut seg: Vec<pdf_inspector::TextItem> = Vec::new();
        if let Some(s) = split {
            let mut split_idx: Option<usize> = None;
            let mut best_dist = f32::INFINITY;
            for i in 1..sorted.len() {
                let gap = sorted[i].x - (sorted[i - 1].x + sorted[i - 1].width);
                if gap > MIN_GAP_FRACTION * page_w {
                    let mid = (sorted[i - 1].x + sorted[i - 1].width + sorted[i].x) / 2.0;
                    let d = (mid - s).abs();
                    if d < best_dist {
                        best_dist = d;
                        split_idx = Some(i);
                    }
                }
            }
            if let Some(idx) = split_idx {
                for item in sorted.drain(..idx) {
                    seg.push(item);
                }
                push_line_region(&seg, line, page, &mut regions);
                seg = sorted;
                push_line_region(&seg, line, page, &mut regions);
                continue;
            }
        }
        seg = sorted;
        push_line_region(&seg, line, page, &mut regions);
    }

    // B3-T：标题级别判定统一于 `text_health::title_levels`
    // （空 hints + numbering=true，纯编号启发式，与 OFD 文字层同口径）。
    // #6 第 2 步：这里只**赋级别**，`#` 前缀与标题前后空行由 docir 渲染层写出
    // （TextLayerPdf 分支），故 `Region.text` 是未加前缀的行文本。
    // #11b：走 boxed 链路——`regions`（push_line_region 造的）本就带几何，
    // 此前经 String 薄封装全丢了 → content_list v2 无 bbox。
    // #11c：末尾追加**段落合并**。顺序 order → postprocess → merge：连字符合并
    // 必须先于段落合并（合并后行尾连字符埋进段落中部，行对不再相邻）。合并判据
    // 复用 `merge_into_paragraphs` 的 median_gap×1.5（与 OCR 通路块内合并同真相）；
    // 拼接统一 MinerU 行语境规则（西方语境补空格、下行字母 CJK 语境不加）——
    // #11c-v2 起 OCR 通路同档，Concat 分档已删（三通路一个真相）。
    // 标题行强制独段、阅读序的列边界/页眉页脚 gap 突变自然断段。GJB 真实样本
    // 实测：长段落 1 → 多段粒度恢复（BACKLOG #11c）。
    let boxed = reading_order::merge_into_paragraphs(&reading_order::postprocess_lines_boxed(
        reading_order::order_text_regions_boxed(&regions),
    ));
    let lines: Vec<String> = boxed.iter().map(|l| l.text.clone()).collect();
    let levels = crate::text_health::title_levels(&lines, &[], true);
    // #11c-v3 附票：字号信号补位——无编号大字号行（护栏已独段）赋 title 级别
    // （对齐 MinerU basic 的 doc_title `#` / paragraph_title `##`）。
    let sizes: Vec<Option<f32>> = boxed.iter().map(|l| l.font_size).collect();
    let levels = crate::text_health::merge_font_levels(levels, &sizes);
    crate::text_health::body_regions_boxed(boxed, levels)
}

/// 从每行内找出"列间隙"候选（gap 中点），按 x 聚类；主簇 >=3 行才返回全局 split_x。
///
/// 双列页：每行的 gutter 都在同一 x → 聚成主簇。封面大标题字母间距大但每行
/// split_x 不同/行数少 → 主簇不足 3 → 返回 None，行保持整行。
fn clustered_row_split(lines: &[pdf_inspector::extractor::TextLine], page_w: f32) -> Option<f32> {
    let min_gap = COL_GUTTER_GAP_FRACTION * page_w;
    let tol = SPLIT_CLUSTER_TOL_FRACTION * page_w;
    let mut candidates: Vec<f32> = Vec::new();
    for line in lines {
        let mut sorted = line.items.clone();
        sorted.sort_by(|a, b| a.x.total_cmp(&b.x));
        let mut best_gap = min_gap;
        let mut best_mid: Option<f32> = None;
        for i in 1..sorted.len() {
            let gap = sorted[i].x - (sorted[i - 1].x + sorted[i - 1].width);
            if gap > best_gap {
                best_gap = gap;
                best_mid = Some((sorted[i - 1].x + sorted[i - 1].width + sorted[i].x) / 2.0);
            }
        }
        if let Some(mid) = best_mid {
            candidates.push(mid);
        }
    }
    if candidates.len() < 3 {
        return None;
    }
    candidates.sort_by(|a, b| a.total_cmp(b));
    let mut clusters: Vec<Vec<f32>> = Vec::new();
    for c in candidates {
        if let Some(last) = clusters.last_mut() {
            if (last[0] - c).abs() <= tol {
                last.push(c);
            } else {
                clusters.push(vec![c]);
            }
        } else {
            clusters.push(vec![c]);
        }
    }
    let dominant = clusters.iter().max_by_key(|c| c.len())?;
    (dominant.len() >= 3).then(|| dominant.iter().sum::<f32>() / dominant.len() as f32)
}

/// 表格候选启发式（R1 信号2）：某页是否"疑似表格"。
///
/// 规则：存在 >=3 行，每行被宽间隙（>1% 页宽，与列检测同口径）拆成 >=3 个
/// x 分离段 → 疑似表格。保守设计：双列正文每行只有 1 条 gutter → 2 段，
/// 永远够不到 3 段；封面/标题的字母间距是单行现象，行数不足 3。真表格行
/// 多为多列（>=3 段）且跨多行对齐 → 命中。误报也无妨：命中页会走 Table
/// 版面 OCR，最终以 `LayoutElementType::Table` 确认，未确认即回落文字层。
fn page_has_tabular_rows(lines: &[pdf_inspector::extractor::TextLine], page_w: f32) -> bool {
    let min_gap = MIN_GAP_FRACTION * page_w;
    let mut multi_seg_rows = 0usize;
    for line in lines {
        let mut sorted = line.items.clone();
        sorted.sort_by(|a, b| a.x.total_cmp(&b.x));
        let mut segs = 1usize;
        for i in 1..sorted.len() {
            let gap = sorted[i].x - (sorted[i - 1].x + sorted[i - 1].width);
            if gap > min_gap {
                segs += 1;
            }
        }
        if segs >= 3 {
            multi_seg_rows += 1;
            if multi_seg_rows >= 3 {
                return true;
            }
        }
    }
    false
}

/// R3 末页探针：在末页全页区域内跑一次 pdf-inspector 表格提取。
///
/// 布局模型对页脚版权栏这类小表格常漏检；此探针用 pdf-inspector 的
/// rect→line→启发式检测兜底，命中返回管道表 markdown。区域坐标为
/// PDF 点、top-left 原点（`extract_tables_in_regions_mem` 约定），
/// 宽/高加 40pt 余量防边缘裁剪。任何失败/空结果 → `None`，不影响主流程。
///
/// T11：上游只有 `&[u8]` 接口，原实现 `fs::read` 会把整个 PDF 载入堆
/// （500MB 文件 → 500MB 峰值）。改为 mmap 只读映射，页面按需换入、
/// 不占堆；映射失败（无 mmap 的文件系统等）回落一次性整读。
fn probe_last_page_table(
    path: &Path,
    last_page: u32,
    page_w: &BTreeMap<u32, f32>,
    page_h: &BTreeMap<u32, f32>,
) -> Option<String> {
    let pdf_bytes = open_pdf_bytes(path)?;
    let w = page_w.get(&last_page).copied().unwrap_or(595.0);
    let h = page_h.get(&last_page).copied().unwrap_or(842.0);
    let regions = [(
        last_page.saturating_sub(1),
        vec![[0.0, 0.0, w + 40.0, h + 40.0]],
    )];
    let results =
        pdf_inspector::extract_tables_in_regions_mem(pdf_bytes.as_slice(), &regions).ok()?;
    let md = results.into_iter().next()?.regions.into_iter().next()?.text;
    let md = md.trim();
    (!md.is_empty()).then(|| md.to_string())
}

/// 只读打开 PDF 字节的载体：优先 mmap（零堆拷贝，T11），失败回落整读。
enum PdfBytes {
    Mapped(memmap2::Mmap),
    Owned(Vec<u8>),
}
impl PdfBytes {
    fn as_slice(&self) -> &[u8] {
        match self {
            PdfBytes::Mapped(m) => m.as_ref(),
            PdfBytes::Owned(v) => v,
        }
    }
}

/// 只读打开 PDF 字节：优先 mmap，映射失败（无 mmap 的文件系统等）回落整读。
/// 返回 `None` 表示文件不可读（调用方据此跳过相应兜底逻辑）。
fn open_pdf_bytes(path: &Path) -> Option<PdfBytes> {
    if let Ok(f) = std::fs::File::open(path) {
        // SAFETY: 只读映射输入 PDF。若外部进程在转换期间截断/改写该文件，对
        // 已映射页的访问可能触发 SIGBUS（mmap 语义，进程终止）或读到不一致
        // 字节——这是 mmap 读者的固有风险，非本工具引入；此处仅用于解析，
        // 文件被并发改写属调用方环境问题。Linux 下映射不依赖 fd 存活。
        if let Ok(m) = unsafe { memmap2::Mmap::map(&f) } {
            return Some(PdfBytes::Mapped(m));
        }
    }
    std::fs::read(path).ok().map(PdfBytes::Owned)
}

/// 坏字体乱码检测：前 4000 个 TextItem 中替换符 `\u{FFFD}`、私有区
/// (U+E000..=U+F8FF)、控制字符占比达 20% 且字符总数 >50 → 判定乱码，
/// 文字层应回退 OCR。字符分类逻辑收敛于 `text_health`。
fn looks_garbled(items: &[pdf_inspector::TextItem]) -> bool {
    let chars = items
        .iter()
        .take(GARBLED_MAX_ITEMS)
        .flat_map(|it| it.text.chars());
    crate::text_health::has_garbled_chars(
        chars,
        crate::text_health::GARBLED_MIN_TOTAL_CHARS,
        crate::text_health::GARBLED_BAD_PERCENT_THRESHOLD,
    )
}

/// 跨页重复"页面家具/水印"检测：返回需剔除的 TextItem 签名集合
/// `(page, x_bits, y_bits, text)`（x/y 用 `f32::to_bits()` 存——`f32` 不满足
/// `Eq`/`Hash`，不能直接作为 `HashSet` 元素；位模式保精度、去重精确）。
///
/// 判定规则：trim 后文本相同，且在 >= `pages_needed` 个不同页面出现在相似
/// 归一化位置（x 中心、y 各自 1% 箱内）→ 页眉/页脚/居中/斜向水印等重复家具。
///
/// 归一化：每页页宽≈max(x+width)、页高≈max(y+height)（PDF y 原点左下）；
/// x_norm=(x+width/2)/页宽，y_norm=(y+height/2)/页高，再取 1% 箱
/// `(x_norm*100) as i32, (y_norm*100) as i32`。
///
/// `page_total < pages_needed`（如单页文档）直接返回空集 → 零误杀。
fn is_repeated_furniture(
    items: &[pdf_inspector::TextItem],
    pages_needed: usize,
    page_total: usize,
) -> HashSet<(u32, u32, u32, String)> {
    if items.is_empty() || page_total < pages_needed {
        return HashSet::new();
    }
    // 每页近似页宽/页高
    let mut page_max_x: BTreeMap<u32, f32> = BTreeMap::new();
    let mut page_max_y: BTreeMap<u32, f32> = BTreeMap::new();
    for it in items {
        let xr = it.x + it.width;
        let yr = it.y + it.height;
        page_max_x
            .entry(it.page)
            .and_modify(|m| *m = m.max(xr))
            .or_insert(xr);
        page_max_y
            .entry(it.page)
            .and_modify(|m| *m = m.max(yr))
            .or_insert(yr);
    }
    let bin = |it: &pdf_inspector::TextItem| -> (i32, i32) {
        let mx = page_max_x.get(&it.page).copied().unwrap_or(1.0).max(1.0);
        let my = page_max_y.get(&it.page).copied().unwrap_or(1.0).max(1.0);
        let x_norm = (it.x + it.width / 2.0) / mx;
        let y_norm = (it.y + it.height / 2.0) / my;
        ((x_norm * 100.0) as i32, (y_norm * 100.0) as i32)
    };
    // key = (trimmed_text, x_bin, y_bin) → 出现过的不同页集合
    let mut key_pages: BTreeMap<(String, i32, i32), BTreeSet<u32>> = BTreeMap::new();
    for it in items {
        let text = it.text.trim();
        if text.is_empty() {
            continue;
        }
        let (xb, yb) = bin(it);
        key_pages
            .entry((text.to_string(), xb, yb))
            .or_default()
            .insert(it.page);
    }
    let furniture: HashSet<(String, i32, i32)> = key_pages
        .into_iter()
        .filter(|(_, pages)| pages.len() >= pages_needed)
        .map(|(k, _)| k)
        .collect();
    items
        .iter()
        .filter(|it| {
            let (xb, yb) = bin(it);
            furniture.contains(&(it.text.trim().to_string(), xb, yb))
        })
        .map(|it| (it.page, it.x.to_bits(), it.y.to_bits(), it.text.clone()))
        .collect()
}

/// 把一段（列内）TextItem 组行并转为 region。复用 pdf-inspector 的文本拼接。
fn push_line_region(
    seg: &[pdf_inspector::TextItem],
    template: &pdf_inspector::extractor::TextLine,
    page: u32,
    regions: &mut Vec<Region>,
) {
    if seg.is_empty() {
        return;
    }
    let line = pdf_inspector::extractor::TextLine {
        items: seg.to_vec(),
        y: template.y,
        page,
        adaptive_threshold: template.adaptive_threshold,
    };
    // 行内样式（`**`/`<u>` 字面量）曾由已废弃的 `ANYDOC_RICH_TEXT` 经
    // `TextLine::text_with_formatting` 注入；样式改由结构化 span 承载
    // （#6 第 4 步），故这里恒走 `text()`。
    let text = line.text();
    let text = text.trim().to_string();
    if text.is_empty() {
        return;
    }
    let spans = build_spans(seg);
    let mut x_min = f32::INFINITY;
    let mut x_max = f32::NEG_INFINITY;
    let mut y_max_pdf = f32::NEG_INFINITY;
    for item in &line.items {
        x_min = x_min.min(item.x);
        x_max = x_max.max(item.x + item.width);
        y_max_pdf = y_max_pdf.max(item.y);
    }
    // PDF 坐标原点左下（y 大=靠上）。reading_order 约定 y 越小越靠上，翻转：-y。
    let y_flip = -line.y;
    // #11c-v3：行字号 = 行内 max em 高度（max 对上/下标天然免疫——它们字号
    // 更小；旋转 run 的 font_size 语义由 pdf-inspector 保证为 em 高度）。
    let font_size = line.items.iter().map(|i| i.font_size).fold(f32::NAN, f32::max);
    let font_size = if font_size.is_finite() { Some(font_size) } else { None };
    regions.push(
        Region::new(
            x_min,
            x_max,
            y_flip,
            y_flip + (y_max_pdf - line.y).max(1.0),
            text,
        )
        .with_spans(spans)
        .with_font_size(font_size),
    );
}

/// #6 第 4 步：把一行的 TextItem 序列切成**样式连续**的 span 段。
///
/// 分组键 = `(is_bold, is_italic, is_underline, is_strikeout, baseline_shift 符号)`；
/// 相邻同键 item 合并为一个 span，span 文本 = item 原文原样拼接（保留 item 内部
/// 与边缘空白）。跨段空格：前段尾与后段首均非空白、且几何间隙 ≥ 0.2 em 时，把
/// 空格归到**前段尾**（词空格经验阈值，对齐 pdf-inspector `SCRIPT_WORD_GAP` 注释
/// 里 "a word space is ≥ 0.2 em" 的口径）。
///
/// **刻意不复刻** `TextLine::text_plain` 的完整插空规则（script 边缘、连字符、
/// 堆叠分数、单字符阈值……）：那些判定是 pdf-inspector 的私有实现，复刻即双源
/// 漂移。词间空格的**真相仍是 `Region.text`**（含 text_plain 的 `<sup>/<sub>`
/// 标签与全部插空）；spans 承载的是样式与 run 边界，投影层（#11）读样式时以
/// text 为文本真相、以 spans 为样式真相。两层的内容一致性（剥标签 + 剥空白后
/// 相等）由单测 `spans_join_matches_text_ignoring_tags_and_spaces` 钉住。
fn build_spans(seg: &[pdf_inspector::TextItem]) -> Vec<Span> {
    use crate::region::SpanStyles;

    #[derive(PartialEq)]
    struct Key {
        bold: bool,
        italic: bool,
        underline: bool,
        strikeout: bool,
        superscript: bool,
        subscript: bool,
    }

    impl Key {
        fn of(it: &pdf_inspector::TextItem) -> Self {
            Key {
                bold: it.is_bold,
                italic: it.is_italic,
                underline: it.is_underline,
                strikeout: it.is_strikeout,
                superscript: it.baseline_shift > 0.0,
                subscript: it.baseline_shift < 0.0,
            }
        }

        fn styles(&self) -> SpanStyles {
            SpanStyles {
                bold: self.bold,
                italic: self.italic,
                underline: self.underline,
                strikethrough: self.strikeout,
                superscript: self.superscript,
                subscript: self.subscript,
            }
        }
    }

    let mut spans: Vec<Span> = Vec::new();
    let mut cur_key: Option<Key> = None;
    let mut cur_text = String::new();
    // 组内最后一个非全空白 item（跨组空格判定的"前段尾"）。
    let mut last_item: Option<&pdf_inspector::TextItem> = None;

    let flush = |key: &Key, text: &mut String, spans: &mut Vec<Span>| {
        if !text.is_empty() {
            spans.push(Span::new(std::mem::take(text), key.styles()));
        }
    };

    for item in seg.iter() {
        let key = Key::of(item);
        match &cur_key {
            Some(k) if *k == key => {}
            Some(k) => {
                // 样式切换：跨段空格判定（前段尾非空白 && 后段首非空白 &&
                // 几何间隙 ≥ 0.2 em → 空格归前段尾）。
                let gap = match (last_item, item) {
                    (Some(p), c) if c.x >= p.x + p.width => c.x - (p.x + p.width),
                    _ => 0.0,
                };
                let em = match (last_item, item) {
                    (Some(p), c) => p.font_size.max(c.font_size),
                    _ => 0.0,
                };
                let tail_blank = cur_text.ends_with(char::is_whitespace);
                let head_blank = item.text.starts_with(char::is_whitespace);
                if !tail_blank
                    && !head_blank
                    && !cur_text.is_empty()
                    && !item.text.is_empty()
                    && gap >= 0.2 * em
                {
                    cur_text.push(' ');
                }
                flush(k, &mut cur_text, &mut spans);
                cur_key = Some(key);
            }
            None => {
                cur_key = Some(key);
            }
        }
        cur_text.push_str(&item.text);
        if !item.text.trim().is_empty() {
            last_item = Some(item);
        }
    }
    if let Some(k) = &cur_key {
        flush(k, &mut cur_text, &mut spans);
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::{
        LayerHit, PageBox, build_body_regions, build_line_groups, build_text_docir,
        clustered_row_split, drop_oversized_pages, hybrid_disabled_from, is_repeated_furniture,
        looks_garbled,
    };
    use std::collections::{BTreeMap, HashMap};
    use crate::docir::{DocIR, PageSource};
    use crate::region::{Region, RegionKind};
    use crate::table_grid::{TableCell, TableGrid};
    use pdf_inspector::TextItem;
    use pdf_inspector::extractor::TextLine;

    const PAGE_W: f32 = 595.0; // A4 宽（pt）

    fn ti(text: &str, x: f32, width: f32) -> TextItem {
        TextItem {
            text: text.to_string(),
            x,
            y: 0.0,
            width,
            height: 10.0,
            advance_known: true,
            font: "test".into(),
            font_size: 10.0,
            page: 1,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: false,
            item_type: Default::default(),
            mcid: None,
            rotation: 0.0,
            font_tag: String::new(),
            legacy_symbol_rewrite: false,
            font_weight: None,
            bold_source: None,
            fixed_pitch: None,
            fill_color: None,
            stroke_color: None,
            render_mode: None,
            baseline_shift: 0.0,
        }
    }

    /// 带页面/坐标的 TextItem 构造（家具检测测试用）。
    fn tif(text: &str, x: f32, y: f32, w: f32, h: f32, page: u32) -> TextItem {
        TextItem {
            text: text.to_string(),
            x,
            y,
            width: w,
            height: h,
            advance_known: true,
            font: "test".into(),
            font_size: 10.0,
            page,
            is_bold: false,
            is_italic: false,
            is_underline: false,
            is_strikeout: false,
            item_type: Default::default(),
            mcid: None,
            rotation: 0.0,
            font_tag: String::new(),
            legacy_symbol_rewrite: false,
            font_weight: None,
            bold_source: None,
            fixed_pitch: None,
            fill_color: None,
            stroke_color: None,
            render_mode: None,
            baseline_shift: 0.0,
        }
    }

    /// 带样式证据的 TextItem 构造（#6 第 4 步 spans 测试用；其余字段同 ti）。
    fn tis(text: &str, x: f32, width: f32, bold: bool, italic: bool, shift: f32) -> TextItem {
        TextItem {
            is_bold: bold,
            is_italic: italic,
            baseline_shift: shift,
            ..ti(text, x, width)
        }
    }

    fn tl(items: Vec<TextItem>) -> TextLine {
        TextLine {
            items,
            y: 0.0,
            page: 1,
            adaptive_threshold: 0.0,
        }
    }

    /// 双列正文：左列 x≈50..65，右列 x≈340..355，gutter 中点 ≈202.5。
    /// 4 行重复同一 gutter → 主簇 >=3 → 返回 ≈202.5。
    #[test]
    fn two_column_rows_return_gutter_midpoint() {
        let lines: Vec<TextLine> = (0..4)
            .map(|_| {
                tl(vec![
                    ti("左", 50.0, 5.0),
                    ti("列", 55.0, 5.0),
                    ti("文", 60.0, 5.0),
                    ti("右", 340.0, 5.0),
                    ti("列", 345.0, 5.0),
                    ti("文", 350.0, 5.0),
                ])
            })
            .collect();
        let split = clustered_row_split(&lines, PAGE_W).expect("应检测到双列 gutter");
        assert!((split - 202.5).abs() < 1e-3, "split={split}");
    }

    /// 标题行字母间距大（每行 split_x 不同）+ 两行各自不同的大间隙 → 每簇仅 1 行，
    /// 无 >=3 主簇 → None，标题行保持整行。
    #[test]
    fn scattered_gaps_return_none() {
        // 标题行：等宽字母间距 20（> min_gap 5.95），只产生 1 个候选
        let title = tl(vec![
            ti("T", 50.0, 10.0),
            ti("I", 80.0, 10.0),
            ti("T", 110.0, 10.0),
            ti("L", 140.0, 10.0),
        ]);
        // 另两行：间隙不同 x 处，各自形成独立簇
        let row2 = tl(vec![ti("a", 50.0, 20.0), ti("b", 250.0, 20.0)]);
        let row3 = tl(vec![ti("c", 60.0, 20.0), ti("d", 300.0, 20.0)]);
        let split = clustered_row_split(&[title, row2, row3], PAGE_W);
        assert_eq!(split, None);
    }

    /// 候选行 <3 → None。
    #[test]
    fn fewer_than_three_candidate_rows_return_none() {
        let lines: Vec<TextLine> = (0..2)
            .map(|_| {
                tl(vec![
                    ti("左", 50.0, 5.0),
                    ti("列", 55.0, 5.0),
                    ti("右", 340.0, 5.0),
                    ti("列", 345.0, 5.0),
                ])
            })
            .collect();
        assert_eq!(clustered_row_split(&lines, PAGE_W), None);
    }

    /// 大量替换符 \u{FFFD}（占比 50% > 20%）→ 乱码。
    #[test]
    fn many_replacement_chars_is_garbled() {
        let items = vec![ti(
            &format!("{}{}", "a".repeat(30), "\u{FFFD}".repeat(30)),
            0.0,
            10.0,
        )];
        assert!(looks_garbled(&items));
    }

    /// 正常 CJK/Latin 文本 → 非乱码。
    #[test]
    fn normal_text_is_not_garbled() {
        let items = vec![ti(
            "你好，世界 Hello World, this is a normal sentence.",
            0.0,
            10.0,
        )];
        assert!(!looks_garbled(&items));
    }

    /// 同文本 + 同归一化位置出现在 5/6 页（pages_needed=4）→ 判为家具，签名剔除。
    /// 正文每页不同（真实正文如此），不受影响。
    #[test]
    fn same_text_same_position_on_most_pages_is_furniture() {
        let mut items: Vec<TextItem> = Vec::new();
        for page in 1..=6u32 {
            // 正文每页不同，保证各页 page_max 一致
            items.push(tif(
                &format!("正文内容第{page}页"),
                100.0,
                400.0,
                200.0,
                10.0,
                page,
            ));
            if page <= 5 {
                // 页眉：页 1..5 同一位置
                items.push(tif(
                    "上海市人民政府公报 2025·1",
                    200.0,
                    800.0,
                    100.0,
                    10.0,
                    page,
                ));
            }
        }
        let drop = is_repeated_furniture(&items, 4, 6);
        // 5 个页眉全部命中
        assert_eq!(drop.len(), 5, "drop={drop:?}");
        for page in 1..=5u32 {
            assert!(drop.contains(&(
                page,
                200.0f32.to_bits(),
                800.0f32.to_bits(),
                "上海市人民政府公报 2025·1".to_string()
            )));
        }
        // 正文不受影响
        for page in 1..=6u32 {
            assert!(!drop.contains(&(
                page,
                100.0f32.to_bits(),
                400.0f32.to_bits(),
                format!("正文内容第{page}页")
            )));
        }
    }

    /// 同文本但每页位置不同（x 超出 1% 箱）→ 每个 key 仅 1 页 → 非家具。
    #[test]
    fn same_text_different_position_per_page_not_furniture() {
        let mut items: Vec<TextItem> = Vec::new();
        for page in 1..=6u32 {
            items.push(tif(
                &format!("正文内容第{page}页"),
                100.0,
                400.0,
                200.0,
                10.0,
                page,
            ));
            items.push(tif(
                "WATERMARK",
                50.0 + page as f32 * 100.0,
                800.0,
                30.0,
                10.0,
                page,
            ));
        }
        let drop = is_repeated_furniture(&items, 4, 6);
        assert!(drop.is_empty(), "drop={drop:?}");
    }

    /// 文本仅出现在 2 页 → 2 < pages_needed → 非家具；单页文档 → 恒空集。
    #[test]
    fn rare_text_and_single_page_never_filtered() {
        let mut items: Vec<TextItem> = Vec::new();
        for page in 1..=6u32 {
            items.push(tif(
                &format!("正文内容第{page}页"),
                100.0,
                400.0,
                200.0,
                10.0,
                page,
            ));
            if page <= 2 {
                items.push(tif("罕见脚注", 200.0, 50.0, 100.0, 10.0, page));
            }
        }
        assert!(is_repeated_furniture(&items, 4, 6).is_empty());
        // 单页文档：pages_needed=3 > total=1 → 空集
        let single = vec![tif("标题", 200.0, 800.0, 100.0, 10.0, 1)];
        assert!(is_repeated_furniture(&single, 3, 1).is_empty());
    }

    // ── 文字层表格网格重建 + 跨页合并 ──

    /// 编号启发式标题级别（B3-T）：`一、总则`→2，`1.1 适用范围`→3；
    /// 带结束标点的正文不变；`第X章` 不被 `title_level` 识别 → 不变。
    /// 编号启发式标题级别（B3-T）：`一、总则`→2，`1.1 适用范围`→3；
    /// 带结束标点的正文不赋级别；`第X章` 不被 `title_level` 识别 → 不赋级别。
    /// #6 第 2 步：这里断言的是**级别**（IR 数据），渲染视图单独钉一条。
    #[test]
    fn title_levels_by_numbering_heuristic() {
        let lines: Vec<String> = vec![
            "一、总则".into(),
            "这是正文第一句。".into(),
            "1.1 适用范围".into(),
            "第二章 附则".into(),
        ];
        let levels = crate::text_health::title_levels(&lines, &[], true);
        assert_eq!(levels, vec![Some(2), None, Some(3), None]);
        // 渲染视图 = 旧字面量输出（`#` 前缀由 Region::rendered_line 写出）。
        let rendered: Vec<String> =
            crate::text_health::body_regions_boxed(
                crate::reading_order::Line::from_texts(lines.clone()),
                levels,
            )
            .into_iter()
            .map(|r| r.rendered_line().into_owned())
            .collect();
        assert_eq!(
            rendered,
            vec![
                "## 一、总则".to_string(),
                "这是正文第一句。".to_string(),
                "### 1.1 适用范围".to_string(),
                "第二章 附则".to_string(),
            ]
        );
    }

    /// 回归（9001c 文字版 4.1/4.2 正文缺行根因）：单栏页的列表项编号间隙
    /// （`a) `、`b) ` 等，~1% 页宽）不得被当成列间隙聚成假 gutter → 返回 None。
    /// 此前 `MIN_GAP_FRACTION=0.01` 会把 `a)/b)/c)` 与正文间的小间隙判为列间隙，
    /// 多处编号行聚成主簇 → 误判双列 → 正文被拆/颠倒/丢失。
    #[test]
    fn list_label_gaps_do_not_form_false_column() {
        // 单栏：5 个列表项行（编号与正文 gap≈1%），其余为通栏正文行。
        // 通栏正文行无 >3% 间隙 → 主簇候选仅来自列表项 → 3% 阈值下全部被过滤。
        let list_rows: Vec<TextLine> = (0..5)
            .map(|_| {
                tl(vec![
                    ti("a)", 75.0, 12.0),                            // 编号
                    ti("与质量管理体系有关的相关方；", 91.0, 200.0), // 正文，gap≈4pt
                ])
            })
            .collect();
        let body_rows: Vec<TextLine> = (0..10)
            .map(|_| tl(vec![ti("组织应确定与所承担装备任务相关的法律法规、标准、使用需求、保障条件等影响因素。", 75.0, 400.0)]))
            .collect();
        // 通栏正文行无 gap；列表项 gap=(91-87)=4pt，占 595 的 0.67% < 3% → 非候选
        let mut lines = list_rows;
        lines.extend(body_rows);
        assert_eq!(clustered_row_split(&lines, PAGE_W), None);
    }

    // ── 混合路由（anydoc 0.2.4 缺页上报）：缺页判定单测 ──

    fn cell(t: &str) -> TableCell {
        TableCell { text: t.into(), x: 0.0, y: 0.0, h: 10.0 }
    }

    fn body(text: &str) -> Region {
        Region::new(0.0, 100.0, 0.0, 10.0, text)
    }

    fn hit_with(page_count: u32, needs_ocr: &[u32]) -> LayerHit {
        LayerHit { page_count, needs_ocr: needs_ocr.iter().copied().collect(), forced_ocr: Default::default(), select: None }
    }

    /// inspector 多报（实测 `multipage.pdf` 8/8 页全标 scanned）但文字层每页有
    /// 正文 → 缺页恒空 → Complete，纯文字文档行为不变（golden 守护的关键）。
    #[test]
    fn over_reported_needs_ocr_rejected_by_text_coverage() {
        let doc = DocIR {
            pages: (1..=3)
                .map(|p| crate::docir::PageIR {
                    page_no: p,
                    regions: vec![body(&format!("第{p}页正文"))],
                    source: PageSource::TextLayerPdf,
                    dims: crate::docir::PageDims::default(),
                })
                .collect(),
        };
        let h = hit_with(3, &[1, 2, 3]);
        assert_eq!(h.missing_pages(&doc), Vec::<u32>::new());
    }

    /// 真缺页：needs_ocr ∩ 文字层该页无非空内容。空白页与"仅空格"页均算缺；
    /// 越界页号（inspector 报错页 > 实际页数）忽略；返回 1 基升序。
    #[test]
    fn missing_pages_is_intersection_of_needs_ocr_and_empty_pages() {
        let doc = DocIR {
            pages: vec![
                crate::docir::PageIR { page_no: 1, regions: vec![body("有正文")], source: PageSource::TextLayerPdf, dims: crate::docir::PageDims::default() },
                // 页 2 文字层完全无条目（扫描件页）
                crate::docir::PageIR { page_no: 3, regions: vec![body("   ")], source: PageSource::TextLayerPdf, dims: crate::docir::PageDims::default() },
                crate::docir::PageIR { page_no: 4, regions: vec![body("又有正文")], source: PageSource::TextLayerPdf, dims: crate::docir::PageDims::default() },
            ],
        };
        let h = hit_with(4, &[2, 3, 4, 9]);
        assert_eq!(h.missing_pages(&doc), vec![2, 3]);
    }

    /// Grid 区块即使 text 为空也算有内容（表格页不得误判缺页）。
    #[test]
    fn grid_region_counts_as_covered() {
        let grid = TableGrid {
            cols: 2,
            header: vec![],
            rows: vec![vec![cell("a"), cell("b")]],
            has_header: false,
        };
        let doc = DocIR {
            pages: vec![crate::docir::PageIR {
                page_no: 1,
                regions: vec![Region::new(0.0, 0.0, 0.0, 0.0, String::new())
                    .with_kind(RegionKind::Grid(grid))],
                source: PageSource::TextLayerPdf,
                dims: crate::docir::PageDims::default(),
            }],
        };
        let h = hit_with(1, &[1]);
        assert!(h.missing_pages(&doc).is_empty());
    }

    /// 无元数据（page_count=0 / needs_ocr 空）且无字符短路 → 恒不缺页。
    /// （有短路页时即使无 inspector 元数据，短路页也必进缺页集合——见
    /// oversized_page_short_circuits_to_ocr。）
    #[test]
    fn missing_pages_without_metadata_is_empty() {
        let doc = DocIR {
            pages: vec![crate::docir::PageIR {
                page_no: 1,
                regions: vec![],
                source: PageSource::TextLayerPdf,
                dims: crate::docir::PageDims::default(),
            }],
        };
        assert!(hit_with(0, &[1]).missing_pages(&doc).is_empty());
        assert!(hit_with(1, &[]).missing_pages(&doc).is_empty());
    }

    /// 回退开关语义：变量存在即关闭（不限值）；未设置默认开启混合路由。
    #[test]
    fn hybrid_kill_switch_enabled_by_default() {
        assert!(!hybrid_disabled_from(None));
        assert!(hybrid_disabled_from(Some("1")));
        assert!(hybrid_disabled_from(Some("")));
    }

    // ── 审计 #8 附项：单页字符短路 ──

    /// 超限页摘除 → 记入 forced_ocr → 缺页集合含之（即使 inspector 未报）；
    /// 合规页照常走文字层复核，行为不变。
    #[test]
    fn oversized_page_short_circuits_to_ocr() {
        let mut by_page: BTreeMap<u32, Vec<TextItem>> = BTreeMap::new();
        by_page.insert(1, vec![ti("正常页正文", 0.0, 10.0)]);
        by_page.insert(2, vec![ti(&"字".repeat(50), 0.0, 10.0)]); // cap=40 → 超限
        let mut hit = LayerHit::default();
        hit.page_count = 2;
        let kept = drop_oversized_pages(by_page, 40, &mut hit);
        assert_eq!(kept.keys().copied().collect::<Vec<_>>(), vec![1]);
        assert_eq!(hit.forced_ocr.iter().copied().collect::<Vec<_>>(), vec![2]);

        let doc = DocIR {
            pages: kept
                .iter()
                .map(|(&p, _)| crate::docir::PageIR {
                    page_no: p,
                    regions: vec![body("正文")],
                    source: PageSource::TextLayerPdf,
                    dims: crate::docir::PageDims::default(),
                })
                .collect(),
        };
        // inspector 什么都没报（needs_ocr 空）→ 短路页仍进缺页集合。
        assert_eq!(hit.missing_pages(&doc), vec![2]);
    }

    /// 恰好等于上限不触发（`> cap` 语义，与 MinerU 一致）；未超限页零副作用。
    #[test]
    fn oversized_page_boundary() {
        let mut by_page: BTreeMap<u32, Vec<TextItem>> = BTreeMap::new();
        by_page.insert(1, vec![ti(&"字".repeat(40), 0.0, 10.0)]);
        let mut hit = LayerHit::default();
        hit.page_count = 1;
        let kept = drop_oversized_pages(by_page, 40, &mut hit);
        assert!(kept.contains_key(&1));
        assert!(hit.forced_ocr.is_empty());
    }

    // ── --pages：选页域收缩（LayerHit.select）──

    /// 未选页即使被 inspector 标 needs_ocr / 字符短路强制，也绝不允许进
    /// missing_pages——否则 OCR 只覆盖所选页，`got == want` 校验永远补不齐。
    #[test]
    fn missing_pages_respects_selection() {
        let doc = DocIR {
            pages: vec![crate::docir::PageIR {
                page_no: 2,
                regions: vec![],
                source: PageSource::TextLayerPdf,
                dims: crate::docir::PageDims::default(),
            }],
        };
        // 页 1/2/3 都被报 needs_ocr，但只选了 {2} → 缺页只有 2。
        let mut h = hit_with(3, &[1, 2, 3]);
        h.select = Some([2u32].into_iter().collect());
        assert_eq!(h.missing_pages(&doc), vec![2]);
        // 字符短路页同样受选页域约束：forced={1,3}、select={2} → 空。
        let mut h2 = hit_with(3, &[]);
        h2.forced_ocr = [1u32, 3].into_iter().collect();
        h2.select = Some([2u32].into_iter().collect());
        assert!(h2.missing_pages(&doc).is_empty());
        // select=None（未给 --pages）→ 行为与历史一致：forced 全量并入。
        let mut h3 = hit_with(3, &[]);
        h3.forced_ocr = [1u32, 3].into_iter().collect();
        assert_eq!(h3.missing_pages(&doc), vec![1, 3]);
    }

    // `rich_text_switch`（"变量存在即开启"）随 #6 决策 (c) 一并作废：现在这个
    // 判据只服务告警，且住在调度层，测试见 `convert::tests::deprecated_env_*`。
    // 本模块不再有任何样式开关——`push_line_region` 恒走 `line.text()`，
    // 由 `tests/pages_rich_text.rs::rich_text_env_is_a_no_op_with_notice` 从
    // CLI 侧钉住"设了也不出标记"。

    // ── #6 第 1 步 + #11b-v2：PDF 文字层 producer 的 dims 口径 ──

    fn empty_boxes() -> BTreeMap<u32, PageBox> {
        BTreeMap::new()
    }

    /// **无框页**（页树无 MediaBox/CropBox → page_visible_boxes 空）：文字层页
    /// 只能拿到**内容外扩**（max x+width / max y+height）。故 kind 必须是
    /// `ContentExtent`、单位 pt，且 `normalizable() == false`——下游投影据此
    /// 拒绝归一化，而不是拿一个比页面框小的量当分母算出 >1 的 bbox。
    #[test]
    fn text_layer_producer_marks_dims_not_normalizable() {
        let mut by_page: BTreeMap<u32, Vec<TextItem>> = BTreeMap::new();
        by_page.insert(
            1,
            vec![ti("正文", 50.0, 400.0), ti("下行", 50.0, 200.0)],
        );
        let (lines_by_page, page_w, page_h) = build_line_groups(&by_page);
        let doc = build_text_docir(
            &by_page,
            &lines_by_page,
            &page_w,
            &page_h,
            &BTreeMap::new(),
            1,
            None,
            &empty_boxes(),
            &HashMap::new(),
        );
        assert_eq!(doc.pages.len(), 1);
        let dims = &doc.pages[0].dims;
        assert_eq!(dims.kind, crate::docir::PageDimsKind::ContentExtent);
        assert_eq!(dims.unit, crate::docir::PageUnit::Pt);
        assert!(!dims.normalizable(), "内容外扩不得冒充归一化分母");
        // 外扩值本身 = 内容盒右/下边界（50+400 / 0+10）。
        assert!((dims.w - 450.0).abs() < 1e-3, "got {}", dims.w);
        assert!((dims.h - 10.0).abs() < 1e-3, "got {}", dims.h);
    }

    /// #11b-v2 主路径：**有框 + Upright**（rotation map 无此页）→
    /// `PageBoxPdfPt(w, h)`，可归一化——文字层页 bbox 的分母自此落地。
    #[test]
    fn text_layer_producer_uses_page_box_when_upright() {
        let mut by_page: BTreeMap<u32, Vec<TextItem>> = BTreeMap::new();
        by_page.insert(1, vec![ti("正文", 50.0, 400.0)]);
        let (lines_by_page, page_w, page_h) = build_line_groups(&by_page);
        let mut boxes = BTreeMap::new();
        boxes.insert(1, PageBox { x0: 0.0, y0: 0.0, x1: 595.0, y1: 842.0 });
        let doc = build_text_docir(
            &by_page,
            &lines_by_page,
            &page_w,
            &page_h,
            &BTreeMap::new(),
            1,
            None,
            &boxes,
            &HashMap::new(),
        );
        let dims = &doc.pages[0].dims;
        assert_eq!(dims.kind, crate::docir::PageDimsKind::PageBoxPdfPt);
        assert_eq!(dims.unit, crate::docir::PageUnit::Pt);
        assert!(dims.normalizable(), "页框可作归一化分母");
        assert!((dims.w - 595.0).abs() < 1e-3 && (dims.h - 842.0).abs() < 1e-3);
    }

    /// #11b-v2 纪律：**整页转正页**（rotation map 命中，Ccw/Cw）即使有框也
    /// 维持 `ContentExtent`——turned 帧 y 语义与 baseline-flip 换算前提不兼容
    /// 且无实测样本，宁缺勿造（不给 bbox，绝不造数）。
    #[test]
    fn rotated_page_frame_keeps_content_extent() {
        let mut by_page: BTreeMap<u32, Vec<TextItem>> = BTreeMap::new();
        by_page.insert(1, vec![ti("正文", 50.0, 400.0)]);
        let (lines_by_page, page_w, page_h) = build_line_groups(&by_page);
        let mut boxes = BTreeMap::new();
        boxes.insert(1, PageBox { x0: 0.0, y0: 0.0, x1: 595.0, y1: 842.0 });
        let mut rotations = HashMap::new();
        rotations.insert(1, pdf_inspector::PageRotation::Ccw);
        let doc = build_text_docir(
            &by_page,
            &lines_by_page,
            &page_w,
            &page_h,
            &BTreeMap::new(),
            1,
            None,
            &boxes,
            &rotations,
        );
        let dims = &doc.pages[0].dims;
        assert_eq!(dims.kind, crate::docir::PageDimsKind::ContentExtent);
        assert!(!dims.normalizable());
    }

    // ── #11c：文字层段落合并（链路级）──

    /// 直接构造 TextLine 绕过聚行（`ti()` 的 y 恒 0，测不了多行）。
    fn tline(y: f32, text: &str) -> pdf_inspector::extractor::TextLine {
        pdf_inspector::extractor::TextLine {
            items: vec![ti(text, 50.0, 100.0)],
            y,
            page: 1,
            adaptive_threshold: 0.10,
        }
    }

    fn body_texts(regions: &[Region]) -> Vec<String> {
        regions
            .iter()
            .filter(|r| r.kind == crate::region::RegionKind::Body)
            .map(|r| r.text.clone())
            .collect()
    }

    /// 行距均匀（gap 15 < median_gap×1.5）→ 3 行并 1 段；大 gap（170）断段。
    /// 此前文字层每视觉行即一段（GJB 真实样本实测长段 1 vs 扫描版 101）。
    #[test]
    fn text_layer_merges_close_lines_into_paragraphs() {
        let lines = vec![
            tline(400.0, "第一行甲"),
            tline(385.0, "第一行乙"),
            tline(370.0, "第一行丙"),
            tline(200.0, "第二段首行"),
        ];
        let body = body_texts(&build_body_regions(&lines, 1, 595.0));
        assert_eq!(body.len(), 2, "均匀行距 3 行并 1 段 + 大 gap 1 段: {body:?}");
        assert!(body[0].contains("第一行甲") && body[0].contains("第一行丙"), "{body:?}");
        assert_eq!(body[1], "第二段首行");
    }

    /// 标题行强制独段（即使与正文行距均匀）——merge_into_paragraphs 的
    /// is_heading 护栏在文字层链路同样生效。
    #[test]
    fn text_layer_heading_stays_alone_after_merge() {
        let lines = vec![tline(400.0, "1. 总则要求"), tline(385.0, "正文紧随标题")];
        let body = body_texts(&build_body_regions(&lines, 1, 595.0));
        assert_eq!(body.len(), 2, "标题行强制独段: {body:?}");
        assert_eq!(body[0], "1. 总则要求");
    }

    // ── #6 第 2 步：标题级别进 IR、`#` 字面量不进 IR ──

    /// 走**真实 producer 通路**（`build_text_docir`）断言：编号标题被赋
    /// `heading_level`，而 `Region.text` **不含** `#` 字面量——前缀只在渲染层出现。
    /// 这条是"级别是数据、字面量是渲染产物"的唯一端到端钉（其余单测只到函数级）。
    #[test]
    fn producer_stores_level_not_hash_literal() {
        // 两行**不同 y**（同 y 会被 group_into_lines 并成一行）：标题在上、正文在下。
        let mut by_page: BTreeMap<u32, Vec<TextItem>> = BTreeMap::new();
        by_page.insert(
            1,
            vec![
                tif("一、总则", 50.0, 700.0, 80.0, 12.0, 1),
                tif("这是正文第一句。", 50.0, 400.0, 140.0, 12.0, 1),
            ],
        );
        let (lines_by_page, page_w, page_h) = build_line_groups(&by_page);
        let doc = build_text_docir(
            &by_page,
            &lines_by_page,
            &page_w,
            &page_h,
            &BTreeMap::new(),
            1,
            None,
            &empty_boxes(),
            &HashMap::new(),
        );
        let regions = &doc.pages[0].regions;
        assert_eq!(regions.len(), 2, "两行须各自成区，got {regions:?}");
        let heading = regions
            .iter()
            .find(|r| r.text.contains("总则"))
            .expect("标题行应在 IR 里");
        assert_eq!(heading.heading_level, Some(2), "级别进 IR");
        assert!(!heading.text.contains('#'), "字面量不得进 IR: {:?}", heading.text);
        // 正文行不赋级别。
        assert!(
            regions
                .iter()
                .find(|r| r.text.contains("正文第一句"))
                .unwrap()
                .heading_level
                .is_none()
        );
        // 渲染后才有 `##`（幂等：再渲染一次仍是同一份文本，不会叠成 `####`）。
        let once = doc.render();
        assert!(once.contains("## 一、总则"), "got: {once}");
        assert_eq!(once, doc.render(), "渲染无状态，不得叠加前缀");
    }

    // ---- #6 第 4 步：spans（producer→IR 全链） ----

    use super::{build_spans, push_line_region};

    /// 全零样式行：合并为单 span，run 文本原样拼接（无插空：相邻无几何间隙）。
    #[test]
    fn spans_plain_line_is_single_span() {
        let seg = vec![ti("AB", 0.0, 10.0), ti("CD", 10.0, 10.0), ti("EF", 20.0, 10.0)];
        let spans = build_spans(&seg);
        assert_eq!(spans.len(), 1);
        assert!(spans[0].styles.is_plain());
        assert_eq!(spans[0].text, "ABCDEF");
    }

    /// 样式切换切段：normal → bold → 两个 span，各自携带样式位。
    #[test]
    fn style_change_splits_spans() {
        let seg = vec![tis("AB", 0.0, 10.0, false, false, 0.0), tis("CD", 10.0, 10.0, true, false, 0.0)];
        let spans = build_spans(&seg);
        assert_eq!(spans.len(), 2);
        assert!(spans[0].styles.is_plain() && spans[0].text == "AB");
        assert!(spans[1].styles.bold && !spans[1].styles.italic && spans[1].text == "CD");
    }

    /// 上/下标走样式位（baseline_shift 符号），span 文本不含 `<sup>` 标签。
    #[test]
    fn script_runs_become_style_bits_not_tags() {
        let seg = vec![
            tis("word", 0.0, 20.0, false, false, 0.0),
            tis("1", 20.0, 3.0, false, false, 3.0),
            tis("x", 23.0, 4.0, false, false, -2.0),
        ];
        let spans = build_spans(&seg);
        assert_eq!(spans.len(), 3);
        assert!(spans[1].styles.superscript && spans[1].text == "1");
        assert!(spans[2].styles.subscript && spans[2].text == "x");
        assert!(!spans[0].text.contains('<'), "span 文本不带标签");
    }

    /// 跨段几何间隙 ≥ 0.2em → 空格归**前段尾**（英文 bold 词 + 普通词形态）。
    #[test]
    fn inter_span_gap_appends_trailing_space() {
        // 10pt 字号，0.2em = 2pt；间隙 10pt（x: 0..10 → 20..40）。
        let seg = vec![tis("AB", 0.0, 10.0, true, false, 0.0), tis("CD", 20.0, 20.0, false, false, 0.0)];
        let spans = build_spans(&seg);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].text, "AB ");
        assert_eq!(spans[1].text, "CD");
    }

    /// 无间隙的样式切换不插空格（"word" 紧跟斜体段）。
    #[test]
    fn no_gap_no_space_between_spans() {
        let seg = vec![tis("AB", 0.0, 10.0, false, false, 0.0), tis("cd", 10.0, 10.0, false, true, 0.0)];
        let spans = build_spans(&seg);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].text, "AB");
        assert!(spans[1].styles.italic);
    }

    /// 强一致性校验（producer 契约）：spans 拼接**剥标签+剥空白**后与
    /// `line.text()` 剥标签+剥空白相等。text 侧的 `<sup>/<sub>` 标签是
    /// text_plain 写的；span 侧是样式位——两层内容必须同源。
    #[test]
    fn spans_join_matches_text_ignoring_tags_and_spaces() {
        let seg = vec![
            tis("总", 0.0, 10.0, true, false, 0.0),
            tis("则", 10.0, 10.0, true, false, 0.0),
            tis("第", 20.0, 10.0, false, false, 0.0),
            tis("1", 30.0, 3.0, false, false, 3.0),
            tis("条", 33.0, 10.0, false, false, 0.0),
        ];
        let line = tl(seg.clone());
        let spans = build_spans(&seg);
        let strip = |s: &str| {
            s.replace("<sup>", "")
                .replace("</sup>", "")
                .replace("<sub>", "")
                .replace("</sub>", "")
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
        };
        let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(strip(&joined), strip(&line.text()));
    }

    /// 装饰位独立并存：underline 与 strikeout 各归各位（信息不折叠，
    /// pdf-inspector 的 markdown 互斥口径是投影层的事）。
    #[test]
    fn decoration_bits_stay_independent() {
        let mut a = tis("u", 0.0, 5.0, false, false, 0.0);
        a.is_underline = true;
        let mut b = tis("s", 5.0, 5.0, false, false, 0.0);
        b.is_strikeout = true;
        let spans = build_spans(&[a, b]);
        assert_eq!(spans.len(), 2);
        assert!(spans[0].styles.underline && !spans[0].styles.strikethrough);
        assert!(spans[1].styles.strikethrough && !spans[1].styles.underline);
    }

    /// producer→IR 全链：push_line_region 把 spans 挂到 Region 上，
    /// 且 text 与 spans[0].text 同源（单 item 行）。
    #[test]
    fn push_line_region_attaches_spans() {
        let mut regions = Vec::new();
        let template = tl(vec![]);
        let seg = vec![tis("AB", 0.0, 20.0, true, false, 0.0)];
        push_line_region(&seg, &template, 1, &mut regions);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].spans.len(), 1);
        assert!(regions[0].spans[0].styles.bold);
        assert_eq!(regions[0].spans[0].text, "AB");
        assert_eq!(regions[0].text, "AB");
    }

}
