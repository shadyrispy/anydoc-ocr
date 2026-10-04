//! DocIR → GFM 渲染（P1.5/AC-6）：只消费 [`DocIR`]，不依赖 pdf/ofd 内部类型或
//! StructureResult。三源历史装配差异收敛为按 [`PageSource`] 分流的渲染风格，
//! 字节级行为与旧 `emitter` 通路一致（golden 守护，AC-8）：
//!
//! - 正文行（`Body`）：TextLayerPdf/Ocr 保留标题空行语义（`#` 行前后空行），
//!   TextLayerOfd 为朴素 `join("\n")`（历史无空行语义）；
//! - 网格表（`Grid`，跨页合并 pass 已定格在首表页）：文字层源 `html + "\n\n"`
//!   （表格独占页），Ocr 源 `"\n\n" + html + "\n"`（表格与正文共存于首表页）；
//! - 表格 HTML（`TableHtml`，Ocr 源）：正文后空行 + html，多表间空行分隔；
//! - 成品块（`PreRendered`）：producer 已含精确分隔符，原样追加，不二次加工。
//!
//! #6 第 2 步：`#` 前缀从 producer 下移到这里——标题级别是 IR 数据
//! （`Region.heading_level`），字面量由 [`Region::rendered_line`] 写出，空行语义
//! 由 [`Region::is_heading`] 决定。producer 不再往 `text` 里写 `#`，两视图逐条
//! 等价，故**本步输出逐字节不变**（BACKLOG #6 验收判据里的第 2 步）。

use std::collections::BTreeMap;

use crate::docir::{DocIR, PageSource};
use crate::region::{NoiseKind, Region, RegionKind};
use crate::table_grid::table_grid_to_html;

/// 图表占位注释（#10 chart 票）。
///
/// **不带 bbox**（有意的）：markdown 里的坐标既不稳定（随页面尺寸/渲染 DPI
/// 变）又与结构化出口重复——归一化 bbox 由 content_list v2 的 `chart` item
/// 精确承载。裸 token 的好处是**可 grep**（下游按 `<!-- chart -->` 即可数出
/// 一篇文档有几张图被有意略过），与 MinerU 的 `![Chart block](doc:…)` 同属
/// "最小标记"思路。
const CHART_PLACEHOLDER: &str = "<!-- chart -->";

/// 分页标记（#9gap-A）：`<!-- page N of M -->`，对齐 MinerU 4.0.8 basic。
///
/// **为什么需要**（2026-10-04 对拍取证）：MinerU 逐页输出页标记（GJB 38 页
/// 38 个 / nuaa 37 页 37 个），本仓此前零输出。下游拿markdown 无法还原页
/// 边界——跨页表格的续接、图注归属哪一页、页码与页眉的对应关系全都不可知。
///
/// **为什么默认开**：这是与 MinerU 对齐的输出形态，且对**纯消费方零影响**
/// （HTML 注释在 GFM 里不渲染、不进 `content_list v2`）。设
/// `ANYDOC_PAGE_MARKER=0` 可关，供严格逐字节比对的下游回退。
///
/// **页号是 1 基**（`page_no + 1`）：与 MinerU 一致，也符合「`page 1 of 38`」
/// 的人类直觉。`PageIR.page_no` 本身是 0 基（跨文档拼接的内部序号）。
///
/// **空页也出标记**：MinerU 对无内容的页同样输出标记（nuaa 37 页里第 2 页
/// 是空页，标记后紧跟下一个标记）。本仓 `segments` 对 `doc.pages` 每一页都
/// 建条目（`entry().or_default()`，哪怕段为空），故空页会产出「只有标记」的
/// 段——下面用 `total_pages` 单独记总页数，与是否输出该页内容解耦。
///
/// **页号与总数必须同坐标系**（见收尾段的注释）：页号取 `page_no + 1`（真实
/// 页位），总数取 `max(page_no) + 1`（真实末页），不能一个用真实页位、一个用
/// 段数——那会输出 `page 39 of 38` 这种自相矛盾的标记。
fn page_marker(page_index_1based: u32, total_pages: u32) -> String {
    format!("<!-- page {page_index_1based} of {total_pages} -->")
}

/// 图片块占位（#9 gap-B），对齐 MinerU 4.0.8 basic 的 `![Image block](…)`。
///
/// **形态取舍：注释而非 `![](…)`**。MinerU 写 `![Image block](doc:…/page:N/block:M)`
/// 是因为它**产出了图片资产**（存进 doc store）。本仓不落盘图片，写
/// `![](images/…)` 是**死链**——比没有更糟（下游 markdown 渲染出破图）。
/// 故沿用 #10 chart 分支已定的口径：HTML 注释占位，**块内文字不进正文流**。
///
/// 与 `CHART_PLACEHOLDER` 的区别是**带页/块定位**：`<!-- image page:N -->`。
/// 页号让下游能回溯「第几页的图」，这是分页标记（#9 gap-A）带来的额外能力
/// ——正文流里能定位到页，图片占位若不带页号就丢掉了这层信息。
const IMAGE_PLACEHOLDER_PREFIX: &str = "<!-- image page:";
const IMAGE_PLACEHOLDER_SUFFIX: &str = " -->";

/// 渲染选项：#9 gap-A/B 两个新开关的**显式**形态。
///
/// **为什么要显式参数而不是只读env**：`OnceLock` 化的env 读取在同进程内不可
/// 切换，单测就无法同时覆盖「开」与「关」两种形态——16 个既有逐字节断言在
/// 默认开启下全部失败，而它们要验的是**别的**语义（标题空行 / 围栏 / 家具），
/// 不该被迫跟着新开关变。故生产入口 [`render`] / [`render_with_furniture`] 负责
/// 读 env填本结构，测试直接构造本结构精确控制。
#[derive(Clone, Copy, Debug)]
pub(crate) struct RenderOpts {
    /// 输出 `<!-- page N of M -->` 分页标记（#9 gap-A）。
    pub page_marker: bool,
    /// 输出 `<!-- image page:N -->` 图片块占位（#9 gap-B）。
    pub image_marker: bool,
}

impl RenderOpts {
    /// 两侧都关——**既有测试的默认档**，逐字节等于 #9 之前的输出。
    pub(crate) const OFF: Self = Self {
        page_marker: false,
        image_marker: false,
    };

    /// 两侧都开——对齐 MinerU 4.0.8 basic 的生产默认档。
    pub(crate) const ON: Self = Self {
        page_marker: true,
        image_marker: true,
    };

    /// 从环境读取生产默认值。
    pub(crate) fn from_env() -> Self {
        Self {
            page_marker: env_flag("ANYDOC_PAGE_MARKER"),
            image_marker: env_flag("ANYDOC_IMAGE_MARKER"),
        }
    }
}

/// 读「`=0` 才关」的开关。**不缓存**——`OnceLock` 会让同进程内的单测无法
/// 切换，而此处开销是一次 `env::var`（每文档一次，非每行一次），可忽略。
fn env_flag(key: &str) -> bool {
    std::env::var(key).as_deref() != Ok("0")
}

/// 渲染 DocIR 为 GFM 文本：按页分段（页号升序），段间空行，段两端 trim
/// （与旧 `DocumentEmitter::finish` 一致，对齐 GFM 块语义）。
///
/// 选项取生产默认档（env 决定，见 [`RenderOpts::from_env`]）。
pub(crate) fn render(doc: &DocIR) -> String {
    render_with_furniture(doc, false)
}

/// 家具（`Noise`）的**可选输出**渲染（#10 例外项）。
///
/// `emit = false`（默认）：`Noise` 区块零消费（下面各阶段都不匹配它们），
/// "收集进 IR 但不输出"。
///
/// `emit = true`（CLI/env 开关 `ANYDOC_EMIT_FURNITURE`）：每页段末追加 HTML
/// 注释行——`<!-- header: … -->` / `<!-- footer: … -->` /
/// `<!-- page-number: … -->` / `<!-- seal: … -->`。
/// 用注释形态的原因：不污染可见 markdown 文本、可 grep、GFM 合法；正式的
/// 结构化出口是 #10/#11 的 content_list v2 投影（`PAGE_HEADER`/`PAGE_FOOTER`/
/// `PAGE_NUMBER` 独立 item），本分支是过渡形态。
///
/// **Footnote 自 #10 补全（2026-10-01）起正式输出**（`<small>` HTML 形态，
/// 对齐 MinerU `PageFootnoteBlock`），不再受 `emit` 控制、不走注释行。
///
/// 家具项按 `y_min` 升序输出（页眉在前、页脚在后，det 顺序不作保证）。
pub(crate) fn render_with_furniture(doc: &DocIR, emit: bool) -> String {
    render_with_opts(doc, emit, RenderOpts::from_env())
}

/// 占位块的**身份**（#10 chart / #9 gap-B）。
///
/// 为什么要身份而不是 `bool`：渲染循环要判断「当前行与上一行是不是**不同**的
/// 占位块」来决定是否补空行。用 bool（`is_placeholder`）时，
/// `Chart, Image, Chart` 三连占位算成「一直true」→ 一个空行都不补，三个占位
/// 挤成三行；用身份则每次换身份都补，形态与单独出现时一致。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BlockKind {
    Chart,
    Image,
}

/// [`render_with_furniture`] 的**选项显式**版本（见 [`RenderOpts`]）。
pub(crate) fn render_with_opts(doc: &DocIR, emit: bool, opts: RenderOpts) -> String {
    let mut segments: BTreeMap<u32, String> = BTreeMap::new();
    for page in &doc.pages {
        let mut seg = String::new();
        // 1) 正文行：`#` 前缀在此写出（#6 第 2 步下移到渲染层）。
        //    #10 INDEX：目次条目行（`Index`）与正文同道输出（保持阅读顺序），
        //    渲染形态改成 `- ` 列表项（MinerU v1 渲染同样写 `- ` + 可选锚点）。
        //    #10 补全：`Aside`/`Reference` 与正文同道（MinerU `PageAuxTextBlock`
        //    /`RefTextBlock` 的 markdown 形态就是无标记普通段落）；`Code` 走
        //    fenced block（连续 Code 行共享一个围栏，见下方状态机）。
        let bodies: Vec<&Region> = collapse_runs(
            page.regions.iter().filter(|r| is_body_like(&r.kind)).collect(),
            opts.image_marker,
        );
        /// 单个正文/目次行的渲染形态：目次 `- `，图表 `<!-- chart -->`，
        /// 图片 `<!-- image page:N -->`，其余 [`Region::rendered_line`]。
        ///
        /// `image_on == false` 时 Image 行走 `else` 分支返回**原文本**——关掉
        /// 占位不该连带丢掉图内文字（见 [`collapse_runs`] 的取舍说明）。
        fn body_line(r: &Region, page_no: u32, image_on: bool) -> String {
            if r.kind == RegionKind::Index {
                format!("- {}", r.text)
            } else if r.kind == RegionKind::Chart {
                // 丢弃块内文字（图内轴标签/图例/数据标签），只留占位——见
                // `RegionKind::Chart` 文档里与 MinerU basic 的取舍说明。
                CHART_PLACEHOLDER.to_string()
            } else if r.kind == RegionKind::Image && image_on {
                format!("{IMAGE_PLACEHOLDER_PREFIX}{}{IMAGE_PLACEHOLDER_SUFFIX}", page_no + 1)
            } else {
                r.rendered_line().into_owned()
            }
        }
        match page.source {
            // OFD 文字层：朴素单换行拼接（历史行为，无标题空行语义）。
            PageSource::TextLayerOfd => {
                for (i, r) in bodies.iter().enumerate() {
                    if i > 0 {
                        seg.push('\n');
                    }
                    seg.push_str(&body_line(r, page.page_no, opts.image_marker));
                }
            }
            // PDF 文字层 / OCR：标题（渲染后 `#` 开头）前后空行，正文行段落内单换行。
            PageSource::TextLayerPdf | PageSource::Ocr => {
                let mut prev_index = false;
                // #10 chart / #9 gap-B：占位块前后留空行用
                // （`is_placeholder != prev_placeholder` 判据）。Image 占位与
                // Chart 同为独立 HTML 注释块，走同一判据——否则
                // `<!-- image page:7 -->` 会被并进相邻正文段。
                let mut prev_placeholder: Option<BlockKind> = None;
                // #10 补全：Code fence 状态机——进入 Code 输出 ```txt 开栏，
                // 离开输出 ``` 闭栏 + 空行（GFM 围栏块独立）。连续 Code 行共享
                // 一个围栏（MinerU `CodeBody.content` 同为块级原始内容）。
                let mut in_code = false;
                // 闭栏跟随开栏：正文含 ``` 时开栏用 ````，闭栏必须同长，
                // 否则围栏提前终止（MinerU `_render_fenced_content` 同口径）。
                let mut cur_fence = "```";
                for r in &bodies {
                    let is_code = r.kind == RegionKind::Code;
                    let is_index = r.kind == RegionKind::Index;
                    if is_code && !in_code {
                        in_code = true;
                        if !seg.is_empty() && !seg.ends_with("\n\n") {
                            seg.push('\n');
                        }
                        // 围栏长于正文中的反引号游程（MinerU
                        // `_render_fenced_content` 同口径）；语言本仓无判别器，恒 txt。
                        cur_fence = if r.text.contains("```") { "````" } else { "```" };
                        seg.push_str(cur_fence);
                        seg.push_str("txt\n");
                    } else if !is_code && in_code {
                        in_code = false;
                        seg.push_str(cur_fence);
                        seg.push_str("\n\n");
                    }
                    // GFM：列表块与前导块之间必须有空行，否则前一段会被吞进列表项。
                    if is_index != prev_index && !seg.is_empty() && !seg.ends_with("\n\n") {
                        seg.push('\n');
                    }
                    // #10 chart / #9 gap-B：占位是独立 HTML 注释块，前后各留
                    // 一个空行（与 Code 围栏块同口径），避免被并进相邻正文段。
                    //
                    // 判据是**块身份**（`Option<BlockKind>`）而非「是不是占位」
                    // （bool）：bool 会让 `Chart, Image, Chart` 三连占位算成
                    // 「一直是占位」而一个空行都不留——三个占位会挤成三行。
                    // 用身份则 Chart→Image→Chart 每次都换身份，各留各的空行。
                    //
                    // **对称**（进入占位与离开占位都补空行），与 `prev_chart` 的
                    // 原语义一致——`<!-- chart -->\n\n图注\n\n<!-- chart -->`。
                    let placeholder = match r.kind {
                        RegionKind::Chart => Some(BlockKind::Chart),
                        RegionKind::Image if opts.image_marker => Some(BlockKind::Image),
                        _ => None,
                    };
                    if placeholder != prev_placeholder && !seg.is_empty() && !seg.ends_with("\n\n") {
                        seg.push('\n');
                    }
                    let is_heading = r.is_heading();
                    if is_heading && !seg.is_empty() && !seg.ends_with("\n\n") {
                        seg.push('\n');
                    }
                    seg.push_str(&body_line(r, page.page_no, opts.image_marker));
                    seg.push('\n');
                    if is_heading {
                        seg.push('\n');
                    }
                    prev_index = is_index;
                    prev_placeholder = placeholder;
                }
                if in_code {
                    // 正文流结束仍有未闭合围栏（页尾即 Code 块尾）
                    seg.push_str(cur_fence);
                    seg.push_str("\n\n");
                }
            }
        }
        // 2) 成品块：原样追加（producer 已嵌入精确分隔符）
        for r in regions_of(page, |k| matches!(k, RegionKind::PreRendered)) {
            seg.push_str(&r.text);
        }
        // 3) OCR 识别表 HTML：正文后空行 + html（多表间以空行分隔）
        if page.source == PageSource::Ocr {
            for r in regions_of(page, |k| matches!(k, RegionKind::TableHtml)) {
                if !seg.ends_with("\n\n") {
                    seg.push_str("\n\n");
                }
                seg.push_str(&r.text);
            }
        }
        // 4) 网格表（跨页合并后定格在本页）
        // #6 第 3 步：带 `continues_prev` 标记的块是"内容已并入前页表格"的占位，
        // **整块跳过**——跳过与旧通路的"物理删除"逐字节相同（单测
        // `absorbed_stub_renders_identically_to_deletion` 钉住），标记只为投影层留。
        for r in regions_of(page, |k| matches!(k, RegionKind::Grid(_)))
            .filter(|r| !r.is_continues_prev())
        {
            let html = grid_html(&r);
            match page.source {
                // 文字层：表格独占一页，html + "\n\n"。
                PageSource::TextLayerPdf | PageSource::TextLayerOfd => {
                    seg.push_str(&html);
                    seg.push_str("\n\n");
                }
                // Ocr：表格与正文共存于首表页，"\n\n" + html + "\n"。
                PageSource::Ocr => {
                    seg.push_str("\n\n");
                    seg.push_str(&html);
                    seg.push('\n');
                }
            }
        }
        // 5) 脚注（#10 补全，2026-10-01）：默认输出 MinerU `PageFootnoteBlock`
        //    的 markdown 形态——非折叠小字号浅色 HTML（`docvortex blocks.py::
        //    _render_page_footnote` 实证）：
        //    `<small><span class="docvortex-page-footnote" data-block-type=
        //    "page_footnote" style="color:#6b7280">…</span></small>`，块内行间
        //    `<br>`（MinerU 把换行统一替换为 <br>）。本仓 Footnote 是行级
        //    Region（无块边界信息）——整页脚注行按 y 序拼进**一个**块。
        //    文本不做 HTML 转义（MinerU 同样原样放行）。
        let mut notes: Vec<&Region> =
            regions_of(page, |k| matches!(k, RegionKind::Footnote)).collect();
        if !notes.is_empty() {
            notes.sort_by(|a, b| {
                a.y_min.partial_cmp(&b.y_min).unwrap_or(std::cmp::Ordering::Equal)
            });
            let joined = notes
                .iter()
                .map(|r| r.text.trim())
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join("<br>");
            if !joined.is_empty() {
                // 正文与脚注间正好一个空行：seg 以单 \n 结尾（正文行循环
                // 常态）补 1 个；无换行结尾（成品块尾部）补 2 个。此前直接
                // push_str("\n\n") 会叠加成 3 个换行。
                if !seg.is_empty() && !seg.ends_with("\n\n") {
                    if !seg.ends_with('\n') {
                        seg.push('\n');
                    }
                    seg.push('\n');
                }
                seg.push_str(&format!(
                    "<small><span class=\"docvortex-page-footnote\" data-block-type=\"page_footnote\" style=\"color:#6b7280\">{joined}</span></small>\n"
                ));
            }
        }
        // 6) 家具注释行（`ANYDOC_EMIT_FURNITURE` 开关）：仅 `Noise` 类（页眉/
        //    页脚/页码/印章）——按 y_min 排序输出注释行；文本中的 `-->` 会
        //    提前终止 HTML 注释，替换为 `->`。Footnote 已正式输出（上一步），
        //    不再走注释形态。
        if emit {
            let mut furniture: Vec<&Region> =
                regions_of(page, |k| matches!(k, RegionKind::Noise(_))).collect();
            furniture.sort_by(|a, b| {
                a.y_min.partial_cmp(&b.y_min).unwrap_or(std::cmp::Ordering::Equal)
            });
            for r in furniture {
                let label = match &r.kind {
                    RegionKind::Noise(NoiseKind::Header) => "header",
                    RegionKind::Noise(NoiseKind::Footer) => "footer",
                    RegionKind::Noise(NoiseKind::PageNumber) => "page-number",
                    RegionKind::Noise(NoiseKind::Seal) => "seal",
                    _ => continue,
                };
                let t = r.text.replace("-->", "->");
                if !seg.is_empty() && !seg.ends_with('\n') {
                    seg.push('\n');
                }
                seg.push_str(&format!("<!-- {label}: {t} -->\n"));
            }
        }
        segments.entry(page.page_no).or_default().push_str(&seg);
    }
    // 收尾：页号升序拼接，段间空行，段两端 trim（旧 finish 语义）。
    //
    // #9 gap-A：页标记插在**每页段首**（空页也出，见 [`page_marker`]）。故这里
    // 不再「空段跳过」——空页要产出「只有标记」的段，否则标记数 ≠ 页数。
    //
    // **`total_pages` 必须与页号同坐标系**。`page_no` 是「跨文档拼接的内部
    // 序号」且**可能稀疏**——GJB 实测：PDF 第 1 页是纯图像封面（无文字层），
    // 装配后 `page_no` 是 2..=39 共 38 段，若总数取 `segments.len()`= 38就会
    // 输出 `<!-- page 39 of 38 -->`（页号 > 总数，自相矛盾）。
    //
    // 故总数取**最大页号 + 1**（真实末页），与页号同坐标系：
    // - 稀疏且末页存在 → `of 39`，「页号 ≤ 总数」恒成立，下游可安全校验。
    // - 连续（0..=n-1）→ `of n`，与 MinerU 逐字节同形。
    //
    // 用`max(1)`兜底空文档（否则 `of 0`）。
    let with_markers = opts.page_marker;
    let total_pages = segments
        .keys()
        .next_back()
        .map_or(1, |last| last.saturating_add(1));
    let mut out = String::new();
    for (page_no, seg) in segments {
        let s = seg.trim();
        if with_markers {
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(&page_marker(page_no + 1, total_pages));
            // 有内容的页：标记后空行 + 内容（与 MinerU 形态一致——标记独占一行）
            if !s.is_empty() {
                out.push_str("\n\n");
                out.push_str(s);
            }
        } else if !s.is_empty() {
            // 关闭标记：逐字节回到旧行为（空段跳过）
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(s);
        }
    }
    out
}

/// 按 kind 谓词依序取区块引用。
fn regions_of(
    page: &crate::docir::PageIR,
    pred: impl Fn(&RegionKind) -> bool,
) -> impl Iterator<Item = &Region> {
    page.regions.iter().filter(move |r| pred(&r.kind))
}

/// 正文同道判定（#10 补全）：按**普通正文流**输出位置的 kind。
/// `Aside`/`Reference` 与 Body 同形态（MinerU markdown 为无标记普通段落），
/// `Code` 也在正文流内原位输出（渲染循环里的 fence 状态机包裹）——四者
/// 与 `Index`（`- ` 列表项）一样保持阅读顺序，不走段末追加。
/// `Chart` 亦在正文流内原位输出（一个 `<!-- chart -->` 注释占位）；
/// `Image` 同理（一个 `<!-- image page:N -->` 注释占位，#9 gap-B）。
fn is_body_like(k: &RegionKind) -> bool {
    matches!(
        k,
        RegionKind::Body
            | RegionKind::Index
            | RegionKind::Aside
            | RegionKind::Reference
            | RegionKind::Code
            | RegionKind::Chart
            | RegionKind::Image
    )
}

/// 把**相邻连续**的 `Chart` 行压成**一个**（保留块内首行，其余丢弃）。
///
/// 一张图在 OCR 侧是**一个** `Chart` 元素 bbox 内的 N 行文字（synth 样本第1 页
/// 实测 N=15：轴刻度/图例/数据标签），而 markdown 端只应留**一个**占位
/// ——对齐 MinerU「一个 ChartBlock 一个块」（`ChartBodyBlock`），也避免输出
/// 15 行重复的 `<!-- chart -->`。
///
/// 判据只有"正文流里相邻连续"（与 content_list 的 `index_run`/`list_run`
/// 聚合同构）：若两张图之间被图注等正文行隔开，就分成两个占位——这正是
/// "两张图"的正确语义。首行代表保留：它的 bbox 是块内首行框，占位落在图的
/// 位置由它决定。
///
/// **非 Chart/Image 行一律原样透传**（不改任何既有 kind 的输出）。
///
/// #9 gap-B 扩展：也把**相邻连续的 `Image` 行**压成一个。语义与 Chart 同构
/// （一个版面 `Image` 元素 bbox 内 N 行文字 → markdown 一个占位）。
///
/// **与 Chart 的关键差异—— `image_on == false` 时不丢行**。
/// Chart 的占位是「这张图的全部内容」的替代物，图内文字（轴标签/图例）在
/// 正文里**没有意义**，所以 Chart 恒丢弃。Image 不同：版面 `Image` 元素 bbox
/// 内的文字可能是**图注/ 说明性正文**（nuaa 第 7 页 PDCA 图内的
/// `起点 终点 输入源…` 就是正文读者需要的），关掉占位不该让这些字消失。
/// 故`image_on == false` 时Image 行**原样透传**（图内文字回到正文流）——
/// 这也让 `ANYDOC_IMAGE_MARKER=0` 严格等于「#9 之前的行为」，与
/// `ANYDOC_PAGE_MARKER=0` 的关闭语义一致：关掉的是**标记**，不是内容。
///
/// 状态用 [`BlockRun`] 单一枚举表达「上一行属于哪个块」——让「丢弃/保留后都
/// 要写状态」这件事在结构上无法漏掉（初版用 `prev_was_chart` + `in_image`
/// 两个 bool，Image 分支的早退 `continue` 漏了写 `prev_was_chart`，序列
/// `Chart, Image, Chart` 会把第二个 Chart 误当续行丢掉）。
fn collapse_runs(bodies: Vec<&Region>, image_on: bool) -> Vec<&Region> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum BlockRun {
        None,
        Chart,
        Image,
    }
    /// `image_on == false` 时 Image **不参与分块**（映射成 `None`）——整块原样
    /// 透传，而不是压成一行：那会把图内 N 行文字砍到只剩 1 行。
    fn run_of(r: &Region, image_on: bool) -> BlockRun {
        match r.kind {
            RegionKind::Chart => BlockRun::Chart,
            RegionKind::Image if image_on => BlockRun::Image,
            _ => BlockRun::None,
        }
    }

    let mut out: Vec<&Region> = Vec::with_capacity(bodies.len());
    let mut prev = BlockRun::None;
    for r in bodies {
        let cur = run_of(r, image_on);
        if cur != BlockRun::None && cur == prev {
            continue; // 同一块的后续行：丢弃（占位已由首行给出）
        }
        out.push(r);
        prev = cur;
    }
    out
}

/// 取 Grid 区块的 HTML（调用处已由谓词保证类型）。
fn grid_html(r: &Region) -> String {
    match &r.kind {
        RegionKind::Grid(g) => table_grid_to_html(g),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docir::{PageDims, PageIR};
    use crate::table_grid::{TableCell, TableGrid};

    fn cell(t: &str) -> TableCell {
        TableCell {
            text: t.into(),
            x: 0.0,
            y: 0.0,
            h: 10.0,
        }
    }

    fn grid(cols: usize, texts: &[&str]) -> TableGrid {
        TableGrid {
            cols,
            header: vec![],
            rows: texts.chunks(cols).map(|c| c.iter().map(|s| cell(s)).collect()).collect(),
            has_header: false,
        }
    }

    fn page(source: PageSource, regions: Vec<Region>) -> PageIR {
        page_at(0, source, regions)
    }

    /// 指定页号的页（#9 gap-A 的分页标记测试需要非 0 / 非连续页号）。
    fn page_at(page_no: u32, source: PageSource, regions: Vec<Region>) -> PageIR {
        PageIR {
            page_no,
            regions,
            source,
            dims: PageDims::default(),
        }
    }

    /// **既有测试的渲染入口**：`RenderOpts::OFF`（#9 两个开关都关）。
    ///
    /// 为什么既有测试不该用 [`render`]（生产默认档）：那 20 处断言验的是标题
    /// 空行 / code 围栏 / 家具注释 / 段落合并等**别的**语义，让它们跟着
    /// #9 gap-A/B 的开关走会让失败信息指向错误的行、掩盖真实回归。#9 自己
    /// 的行为由下面 `page_marker_*` / `image_*` 系列单测显式覆盖（两侧都测）。
    fn r(doc: &DocIR) -> String {
        render_with_opts(doc, false, RenderOpts::OFF)
    }

    /// 家具版`r`：`emit` 显式，其余按 [`r`]。
    fn rf(doc: &DocIR, emit: bool) -> String {
        render_with_opts(doc, emit, RenderOpts::OFF)
    }

    /// PDF/Ocr 正文行：标题（有 `heading_level`）前补空行、后加空行；正文行单换行。
    /// #6 第 2 步：标题是 IR 字段，`#` 字面量由本层写出。
    #[test]
    fn heading_blank_line_semantics_for_pdf_and_ocr() {
        let regions = vec![
            Region::new(0.0, 1.0, 0.0, 1.0, "标题").with_heading_level(Some(2)),
            Region::new(0.0, 1.0, 1.0, 2.0, "正文行"),
            Region::new(0.0, 1.0, 2.0, 3.0, "又一标题").with_heading_level(Some(1)),
        ];
        let doc = DocIR {
            pages: vec![page(PageSource::TextLayerPdf, regions)],
        };
        let out = r(&doc);
        // trim 后：标题行\n\n正文行\n\n# 又一标题（标题前空行体现在块间 \n\n）
        assert!(out.starts_with("## 标题\n\n正文行\n\n# 又一标题"), "got: {out}");
    }

    /// #6 第 2 步的等价性正身：**同一页**用 IR 级别表达标题，与旧通路把 `#`
    /// 字面量焊在 `text` 里，渲染结果必须逐字节相同（三种来源各钉一条）。
    #[test]
    fn level_and_literal_prefix_render_identically() {
        for source in [
            PageSource::TextLayerPdf,
            PageSource::TextLayerOfd,
            PageSource::Ocr,
        ] {
            let by_level = page(
                source,
                vec![
                    Region::new(0.0, 1.0, 0.0, 1.0, "一、总则").with_heading_level(Some(2)),
                    Region::new(0.0, 1.0, 1.0, 2.0, "正文行"),
                ],
            );
            let by_literal = page(
                source,
                vec![
                    Region::new(0.0, 1.0, 0.0, 1.0, "## 一、总则"),
                    Region::new(0.0, 1.0, 1.0, 2.0, "正文行"),
                ],
            );
            assert_eq!(
                r(&DocIR { pages: vec![by_level] }),
                r(&DocIR { pages: vec![by_literal] }),
                "source={source:?}：级别与字面量必须渲染同形"
            );
        }
    }

    /// OFD 正文行：朴素 join("\n")，标题不加空行（前缀照样由本层写）。
    #[test]
    fn ofd_body_joins_with_single_newline() {
        let regions = vec![
            Region::new(0.0, 1.0, 0.0, 1.0, "标题").with_heading_level(Some(2)),
            Region::new(0.0, 1.0, 1.0, 2.0, "正文行"),
        ];
        let doc = DocIR {
            pages: vec![page(PageSource::TextLayerOfd, regions)],
        };
        assert_eq!(r(&doc), "## 标题\n正文行");
    }

    /// 文字层网格表 flush 格式：html + "\n\n"（表格独占页）。
    #[test]
    fn text_layer_grid_flush_format() {
        let regions = vec![Region::new(0.0, 0.0, 0.0, 0.0, String::new())
            .with_kind(RegionKind::Grid(grid(2, &["a", "b"])))];
        let doc = DocIR {
            pages: vec![page(PageSource::TextLayerPdf, regions)],
        };
        let out = r(&doc);
        assert!(out.contains("<table>"), "表格 HTML 输出");
        assert!(out.trim_end().ends_with("</table>"), "段尾 trim 后以 </table> 结束");
    }

    /// Ocr 网格表 flush 格式：正文行尾 `\n` + flush 前缀 `"\n\n"` + html + `"\n"`
    /// （表格与正文共存于首表页；与旧 emitter Gfm flush 字节一致）。
    #[test]
    fn ocr_grid_flush_after_body() {
        let regions = vec![
            Region::new(0.0, 1.0, 0.0, 1.0, "正文"),
            Region::new(0.0, 0.0, 0.0, 0.0, String::new())
                .with_kind(RegionKind::Grid(grid(2, &["a", "b"]))),
        ];
        let doc = DocIR {
            pages: vec![page(PageSource::Ocr, regions)],
        };
        let out = r(&doc);
        let idx = out.find("正文").unwrap();
        assert!(
            out[idx..].starts_with("正文\n\n\n<table"),
            "Ocr 网格表前须空行（正文行尾 \\n + flush \\n\\n），got: {}",
            &out[idx..]
        );
    }

    /// Ocr 表格 HTML：正文行尾 `\n` + `"\n\n"` 补齐 + html；多表间 2 个换行分隔
    /// （与旧 gfm 通路的 `ends_with("\n\n")` 补齐逻辑字节一致）。
    #[test]
    fn ocr_table_html_blank_line_separated() {
        let regions = vec![
            Region::new(0.0, 1.0, 0.0, 1.0, "正文"),
            Region::new(0.0, 0.0, 0.0, 0.0, "<table><tr><td>t1</td></tr></table>")
                .with_kind(RegionKind::TableHtml),
            Region::new(0.0, 0.0, 0.0, 0.0, "<table><tr><td>t2</td></tr></table>")
                .with_kind(RegionKind::TableHtml),
        ];
        let doc = DocIR {
            pages: vec![page(PageSource::Ocr, regions)],
        };
        let out = r(&doc);
        assert!(out.contains("正文\n\n\n<table><tr><td>t1</td></tr></table>"));
        assert!(out.contains("</table>\n\n<table><tr><td>t2</td></tr></table>"));
    }

    /// PreRendered：原样追加（含 producer 嵌入的前导/尾随分隔符）。
    #[test]
    fn pre_rendered_appended_verbatim() {
        let regions = vec![
            Region::new(0.0, 1.0, 0.0, 1.0, "正文行"),
            Region::new(0.0, 0.0, 0.0, 0.0, "\n| 管道表 |\n")
                .with_kind(RegionKind::PreRendered),
        ];
        let doc = DocIR {
            pages: vec![page(PageSource::TextLayerPdf, regions)],
        };
        let out = r(&doc);
        assert!(out.contains("正文行\n\n| 管道表 |"), "got: {out}");
    }

    /// 多页按页号升序拼接、段间空行（旧 finish 语义）。
    #[test]
    fn pages_concatenated_in_order_with_blank_lines() {
        let doc = DocIR {
            pages: vec![
                PageIR {
                    page_no: 2,
                    regions: vec![Region::new(0.0, 1.0, 0.0, 1.0, "third")],
                    source: PageSource::TextLayerOfd,
                    dims: PageDims::default(),
                },
                PageIR {
                    page_no: 0,
                    regions: vec![Region::new(0.0, 1.0, 0.0, 1.0, "first")],
                    source: PageSource::TextLayerOfd,
                    dims: PageDims::default(),
                },
            ],
        };
        assert_eq!(r(&doc), "first\n\nthird");
    }

    /// 空页（无区块）跳过，不产生空段。
    #[test]
    fn empty_pages_skipped() {
        let doc = DocIR {
            pages: vec![
                PageIR {
                    page_no: 0,
                    regions: vec![],
                    source: PageSource::TextLayerOfd,
                    dims: PageDims::default(),
                },
                PageIR {
                    page_no: 1,
                    regions: vec![Region::new(0.0, 1.0, 0.0, 1.0, "内容")],
                    source: PageSource::TextLayerOfd,
                    dims: PageDims::default(),
                },
            ],
        };
        assert_eq!(r(&doc), "内容");
    }

    // ── #10 例外项：家具/脚注的可选输出 ──

    fn noise_region(text: &str, y: f32, kind: RegionKind) -> Region {
        Region::new(0.0, 100.0, y, y + 5.0, text).with_kind(kind)
    }

    fn furniture_doc() -> DocIR {
        DocIR {
            pages: vec![PageIR {
                page_no: 0,
                regions: vec![
                    Region::new(0.0, 100.0, 40.0, 50.0, "正文"),
                    noise_region("页眉文本", 0.0, RegionKind::Noise(NoiseKind::Header)),
                    noise_region("页脚文本", 90.0, RegionKind::Noise(NoiseKind::Footer)),
                    noise_region("第 1 页", 80.0, RegionKind::Noise(NoiseKind::PageNumber)),
                    noise_region("章内散字", 60.0, RegionKind::Noise(NoiseKind::Seal)),
                    noise_region("脚注一行", 70.0, RegionKind::Footnote),
                ],
                source: PageSource::Ocr,
                dims: PageDims::default(),
            }],
        }
    }

    /// 默认渲染：家具（页眉/页脚/页码/印章）零输出；脚注正式输出 `<small>`
    /// HTML（#10 补全，对齐 MinerU `PageFootnoteBlock` 的 markdown 形态）。
    #[test]
    fn furniture_hidden_by_default() {
        assert_eq!(
            r(&furniture_doc()),
            "正文\n\n<small><span class=\"docvortex-page-footnote\" data-block-type=\"page_footnote\" style=\"color:#6b7280\">脚注一行</span></small>"
        );
        // `DocIR::render()` 走生产默认档（读 env，#9 gap-A 默认开）→ 它的输出
        // 是 `r()` **加**页标记前缀。这里只断言「标记在前、内容与 `r()` 逐字节
        // 相同」这个不变量，**不钉死标记是否存在**——钉死会让本测试在
        // `ANYDOC_PAGE_MARKER=0` 下失败，而单测不应依赖调用者的 env。
        let prod = furniture_doc().render();
        let off = r(&furniture_doc());
        assert!(
            prod == off || prod == format!("<!-- page 1 of 1 -->\n\n{off}"),
            "生产档输出既不是 OFF 档、也不是 OFF 档 + 页标记：{prod:?}"
        );
    }

    /// 开关打开：段末注释行按 y 升序（header → seal → page-number → footer）；
    /// `Footnote` 已正式输出（`<small>` HTML），**不再走注释形态**。
    #[test]
    fn furniture_emitted_as_comments_in_y_order() {
        let on = rf(&furniture_doc(), true);
        assert_eq!(
            on,
            "正文\n\
             \n\
             <small><span class=\"docvortex-page-footnote\" data-block-type=\"page_footnote\" style=\"color:#6b7280\">脚注一行</span></small>\n\
             <!-- header: 页眉文本 -->\n\
             <!-- seal: 章内散字 -->\n\
             <!-- page-number: 第 1 页 -->\n\
             <!-- footer: 页脚文本 -->"
        );
    }

    /// 多条脚注行拼进**一个** `<small>` 块，行间 `<br>`（MinerU 把块内换行
    /// 统一替换为 <br>；本仓行级 Region 按全页聚合，y 升序）。
    #[test]
    fn multiple_footnotes_join_into_one_small_block() {
        let doc = DocIR {
            pages: vec![PageIR {
                page_no: 0,
                regions: vec![
                    Region::new(0.0, 100.0, 20.0, 25.0, "正文"),
                    noise_region("注乙", 90.0, RegionKind::Footnote),
                    noise_region("注甲", 80.0, RegionKind::Footnote),
                ],
                source: PageSource::Ocr,
                dims: PageDims::default(),
            }],
        };
        assert_eq!(
            r(&doc),
            "正文\n\n<small><span class=\"docvortex-page-footnote\" data-block-type=\"page_footnote\" style=\"color:#6b7280\">注甲<br>注乙</span></small>"
        );
    }

    /// 文本中的 `-->` 会提前终止 HTML 注释，输出前替换为 `->`。
    #[test]
    fn furniture_text_with_comment_terminator_is_sanitized() {
        let doc = DocIR {
            pages: vec![PageIR {
                page_no: 0,
                regions: vec![noise_region("a --> b", 0.0, RegionKind::Noise(NoiseKind::Header))],
                source: PageSource::Ocr,
                dims: PageDims::default(),
            }],
        };
        assert!(rf(&doc, true).contains("<!-- header: a -> b -->"));
    }

    /// 占位变体（Formula）：零消费——producer 未产，即便有人手工构造也不应
    /// 出现在任何输出里。
    ///
    /// **本用例原先还含 `Image`，#9 gap-B 起该断言被推翻**（`Image` 有了
    /// producer + 消费方，见 `image_placeholder_*` 系列）：`r()`（OFF 档）下
    /// Image 行按**原文本**透传，故这里只留 `Formula`。
    /// `Index` 已非占位（#10 INDEX 票有 producer），改由
    /// [`index_entry_renders_as_list_item`] 钉住；`Code`/`Aside`/
    /// `Reference`/`Chart` 自 #10 补全起有消费（下方用例）。
    #[test]
    fn placeholder_variants_never_render() {
        let doc = DocIR {
            pages: vec![PageIR {
                page_no: 0,
                regions: vec![
                    Region::new(0.0, 1.0, 0.0, 1.0, "式").with_kind(RegionKind::Formula),
                ],
                source: PageSource::Ocr,
                dims: PageDims::default(),
            }],
        };
        assert_eq!(r(&doc), "");
        assert_eq!(rf(&doc, true), "");
    }

    /// #9 gap-B 的**关闭语义**：`ANYDOC_IMAGE_MARKER=0` 时 Image 行按原文本
    /// 透传（占位消失、图内文字回到正文流），**不是**整块丢弃。
    ///
    /// 这条与 `ANYDOC_PAGE_MARKER=0` 的关闭语义一致：关掉的是**标记**，不是
    /// 内容。反过来做（关占位= 丢内容）会让逃生门关掉后输出**比不关更少**。
    #[test]
    fn image_marker_off_keeps_inner_text_as_body() {
        let doc = DocIR {
            pages: vec![page_at(0, PageSource::Ocr, vec![
                Region::new(0.0, 1.0, 0.0, 1.0, "起点终点输入源").with_kind(RegionKind::Image),
                Region::new(0.0, 1.0, 1.0, 2.0, "输入活动输出").with_kind(RegionKind::Image),
            ])],
        };
        // OFF：两行原样透传（不压缩——压成一行会砍掉图内文字）。
        assert_eq!(r(&doc), "起点终点输入源\n输入活动输出");
        // ON：压成一个占位。
        assert_eq!(
            render_with_opts(&doc, false, RenderOpts::ON),
            "<!-- page 1 of 1 -->\n\n<!-- image page:1 -->"
        );
    }

    // ── #10 chart 票：图表注释占位 + 图注保留 + 块内文字丢弃 ──

    /// chart 区域渲染出 `<!-- chart -->` 占位，**块内文字不进正文流**
    /// （`text` 暂存的图内轴标签/图例/数据标签一律不输出）。
    #[test]
    fn chart_renders_placeholder_and_drops_inner_text() {
        let doc = DocIR {
            pages: vec![PageIR {
                page_no: 0,
                regions: vec![
                    Region::new(0.0, 100.0, 0.0, 5.0, "正文一段"),
                    Region::new(0.0, 100.0, 10.0, 15.0, "171.2营业收入")
                        .with_kind(RegionKind::Chart),
                ],
                source: PageSource::Ocr,
                dims: PageDims::default(),
            }],
        };
        let out = r(&doc);
        assert_eq!(out, "正文一段\n\n<!-- chart -->");
        // 块内文字确实没进输出
        assert!(!out.contains("171.2"));
        assert!(!out.contains("营业收入"));
    }

    /// 图注保留：图注是**独立的 FigureTitle 元素**，不在 chart 块内，
    /// 照常走普通正文流，落在占位前后（阅读序原处）。
    #[test]
    fn chart_caption_is_preserved_around_placeholder() {
        let doc = DocIR {
            pages: vec![PageIR {
                page_no: 0,
                regions: vec![
                    Region::new(0.0, 100.0, 0.0, 5.0, "图3-1分季度营业收入与成本对比"),
                    Region::new(0.0, 100.0, 10.0, 15.0, "0").with_kind(RegionKind::Chart),
                    Region::new(0.0, 100.0, 20.0, 25.0, "图3-1分季度营业收入与成本对比 数据来源：内部财务台账"),
                ],
                source: PageSource::Ocr,
                dims: PageDims::default(),
            }],
        };
        let out = r(&doc);
        assert_eq!(
            out,
            "图3-1分季度营业收入与成本对比\n\n<!-- chart -->\n\n图3-1分季度营业收入与成本对比 数据来源：内部财务台账"
        );
        // 图注出现两次是**原文如此**（图上方 + 图下方各一个figure_title），
        // 不是 basic 那种 `chart_caption`/`chart_footnote` 重复渲染同一图注
        // 的缺陷——本仓两个都是真实独立元素。
        assert_eq!(out.matches("图3-1分季度营业收入与成本对比").count(), 2);
    }

    /// 相邻连续的 chart 行只出**一个**占位（一张图 = 一个块，对齐 MinerU
    /// `ChartBlock`）；被正文行隔开则出两个占位（那是两张图）。
    #[test]
    fn consecutive_chart_lines_collapse_to_one_placeholder() {
        let doc = DocIR {
            pages: vec![PageIR {
                page_no: 0,
                regions: vec![
                    Region::new(0.0, 100.0, 10.0, 15.0, "171.2").with_kind(RegionKind::Chart),
                    Region::new(0.0, 100.0, 20.0, 25.0, "营业收入")
                        .with_kind(RegionKind::Chart),
                    Region::new(0.0, 100.0, 30.0, 35.0, "0").with_kind(RegionKind::Chart),
                ],
                source: PageSource::Ocr,
                dims: PageDims::default(),
            }],
        };
        assert_eq!(r(&doc), "<!-- chart -->");

        // 两张图（中间隔一行正文）→ 两个占位
        let doc2 = DocIR {
            pages: vec![PageIR {
                page_no: 0,
                regions: vec![
                    Region::new(0.0, 100.0, 10.0, 15.0, "a").with_kind(RegionKind::Chart),
                    Region::new(0.0, 100.0, 20.0, 25.0, "夹在中间的正文"),
                    Region::new(0.0, 100.0, 30.0, 35.0, "b").with_kind(RegionKind::Chart),
                ],
                source: PageSource::Ocr,
                dims: PageDims::default(),
            }],
        };
        assert_eq!(
            r(&doc2),
            "<!-- chart -->\n\n夹在中间的正文\n\n<!-- chart -->"
        );
    }

    /// 占位**不写** `![](…)` 图片语法——本仓不产图片资产，那会变成死链。
    #[test]
    fn chart_placeholder_is_not_image_markdown() {
        let doc = DocIR {
            pages: vec![PageIR {
                page_no: 0,
                regions: vec![Region::new(0.0, 100.0, 10.0, 15.0, "0")
                    .with_kind(RegionKind::Chart)],
                source: PageSource::Ocr,
                dims: PageDims::default(),
            }],
        };
        let out = r(&doc);
        assert!(!out.contains("!["));
        assert!(!out.contains(".png"));
    }

    // ── #10 补全：aside/reference 普通段落 + code fenced block ──

    /// `Aside`/`Reference` 与正文同道：无标记普通段落（MinerU
    /// `PageAuxTextBlock`/`RefTextBlock` 的 markdown 形态），保持阅读顺序。
    #[test]
    fn aside_and_reference_render_as_plain_paragraphs() {
        let doc = DocIR {
            pages: vec![PageIR {
                page_no: 0,
                regions: vec![
                    Region::new(0.0, 100.0, 0.0, 5.0, "正文一段"),
                    Region::new(0.0, 100.0, 10.0, 15.0, "旁注一行").with_kind(RegionKind::Aside),
                    Region::new(0.0, 100.0, 20.0, 25.0, "〔1〕参考文献条目")
                        .with_kind(RegionKind::Reference),
                    Region::new(0.0, 100.0, 30.0, 35.0, "正文二段"),
                ],
                source: PageSource::Ocr,
                dims: PageDims::default(),
            }],
        };
        assert_eq!(r(&doc), "正文一段\n旁注一行\n〔1〕参考文献条目\n正文二段");
    }

    /// `Code` 渲染 fenced block：连续 Code 行共享一个围栏，语言恒 txt，
    /// 围栏块前后空行（GFM 围栏独立）。行文本原样（保留换行边界）。
    #[test]
    fn code_regions_render_as_fenced_block() {
        let doc = DocIR {
            pages: vec![PageIR {
                page_no: 0,
                regions: vec![
                    Region::new(0.0, 100.0, 0.0, 5.0, "说明文字"),
                    Region::new(0.0, 100.0, 10.0, 15.0, "let x = 1;").with_kind(RegionKind::Code),
                    Region::new(0.0, 100.0, 20.0, 25.0, "let y = 2;").with_kind(RegionKind::Code),
                    Region::new(0.0, 100.0, 30.0, 35.0, "后继正文"),
                ],
                source: PageSource::Ocr,
                dims: PageDims::default(),
            }],
        };
        assert_eq!(
            r(&doc),
            "说明文字\n\n```txt\nlet x = 1;\nlet y = 2;\n```\n\n后继正文"
        );
    }

    /// 围栏长度对齐 MinerU `_render_fenced_content`：正文含 ``` 时用四反引号，
    /// 防围栏提前闭合。
    #[test]
    fn code_containing_backticks_gets_longer_fence() {
        let doc = DocIR {
            pages: vec![PageIR {
                page_no: 0,
                regions: vec![Region::new(0.0, 100.0, 0.0, 5.0, "md```code").with_kind(RegionKind::Code)],
                source: PageSource::Ocr,
                dims: PageDims::default(),
            }],
        };
        assert_eq!(r(&doc), "````txt\nmd```code\n````");
    }

    /// #10 INDEX：目次条目行渲染为 `- ` 列表项（MinerU v1 同形态），且与相邻
    /// 正文块之间有空行——否则 GFM 会把前一段吞进列表项。
    #[test]
    fn index_entry_renders_as_list_item() {
        let doc = DocIR {
            pages: vec![PageIR {
                page_no: 0,
                regions: vec![
                    Region::new(0.0, 1.0, 0.0, 1.0, "目 次").with_heading_level(Some(2)),
                    Region::new(0.0, 1.0, 0.0, 1.0, "前言…………IV").with_kind(RegionKind::Index),
                    Region::new(0.0, 1.0, 0.0, 1.0, "引言…………V").with_kind(RegionKind::Index),
                    Region::new(0.0, 1.0, 0.0, 1.0, "正文第一段。"),
                ],
                source: PageSource::TextLayerPdf,
                dims: PageDims::default(),
            }],
        };
        assert_eq!(
            r(&doc),
            "## 目 次\n\n- 前言…………IV\n- 引言…………V\n\n正文第一段。"
        );
    }

    // ==================== #9 gap-A：分页标记 ====================

    /// 页标记形态：独占一行、两侧空行、页号**1 基**、总数 = 页数。
    #[test]
    fn page_marker_shape_is_one_based_and_total_is_page_count() {
        let doc = DocIR {
            pages: vec![
                page_at(0, PageSource::Ocr, vec![Region::new(0.0, 1.0, 0.0, 1.0, "第一页")]),
                page_at(1, PageSource::Ocr, vec![Region::new(0.0, 1.0, 0.0, 1.0, "第二页")]),
                page_at(2, PageSource::Ocr, vec![Region::new(0.0, 1.0, 0.0, 1.0, "第三页")]),
            ],
        };
        let out = render_with_opts(&doc, false, RenderOpts::ON);
        assert_eq!(
            out,
            "<!-- page 1 of 3 -->\n\n第一页\n\n<!-- page 2 of 3 -->\n\n第二页\n\n<!-- page 3 of 3 -->\n\n第三页"
        );
        // 可校验不变量：标记数 == 页数。
        assert_eq!(out.matches("<!-- page ").count(), doc.pages.len());
    }

    /// **空页也出标记**，且两个标记相邻（MinerU 原文形态：
    /// `'7―01 实施\n\n中央军委装备发展部\n\n<!-- page 2 of 38 -->\n\n<!-- page 3 of 38 -->\n\n…'`）。
    ///
    /// 这条是 gap-A 的核心不变量：沿用旧「空段跳过」会把空页的标记吞掉，
    /// 标记数就 ≠ 页数，下游按 `of M` 定位会错位。
    #[test]
    fn empty_page_still_emits_marker_and_markers_are_adjacent() {
        let doc = DocIR {
            pages: vec![
                page_at(0, PageSource::Ocr, vec![Region::new(0.0, 1.0, 0.0, 1.0, "7―01 实施")]),
                // 空页：无 region
                page_at(1, PageSource::Ocr, vec![]),
                page_at(2, PageSource::Ocr, vec![Region::new(0.0, 1.0, 0.0, 1.0, "中央军委装备发展部")]),
            ],
        };
        let out = render_with_opts(&doc, false, RenderOpts::ON);
        assert_eq!(
            out,
            "<!-- page 1 of 3 -->\n\n7―01 实施\n\n<!-- page 2 of 3 -->\n\n<!-- page 3 of 3 -->\n\n中央军委装备发展部"
        );
        assert_eq!(out.matches("<!-- page ").count(), 3);
    }

    /// 页号与总数**同坐标系**：都用真实页位（`page_no`），而非「页号用真实位+
    /// 总数用段数」——那会输出 `page 39 of 38` 这种页号 > 总数的自相矛盾标记。
    ///
    /// GJB 真实触发场景：PDF 第 1 页是纯图像封面（无文字层），装配后
    /// `page_no` 是 2..=39。若总数取 `segments.len()`= 38就会输出
    /// `<!-- page 39 of 38 -->`。
    #[test]
    fn page_marker_page_no_and_total_share_one_coordinate_system() {
        let doc = DocIR {
            pages: vec![
                page_at(1, PageSource::Ocr, vec![Region::new(0.0, 1.0, 0.0, 1.0, "甲")]),
                page_at(5, PageSource::Ocr, vec![Region::new(0.0, 1.0, 0.0, 1.0, "乙")]),
            ],
        };
        let out = render_with_opts(&doc, false, RenderOpts::ON);
        // 总数 = max(page_no)+1 = 6，与页号同坐标系。
        assert_eq!(
            out,
            "<!-- page 2 of 6 -->\n\n甲\n\n<!-- page 6 of 6 -->\n\n乙",
            "got: {out}"
        );
        // 不变量：任何标记的页号都 ≤ 总数。
        for (n, m) in page_numbers(&out) {
            assert!(n <= m, "页号 {n} > 总数 {m}：{out}");
        }
    }

    /// 空文档：`of 1` 兜底，不输出 `of 0`（否则「页号 ≤ 总数」在无标记时无意义，
    /// 而 `of 0` 对下游是「零页文档」的错误信号）。
    #[test]
    fn empty_doc_page_marker_total_is_one() {
        let doc = DocIR { pages: vec![] };
        assert_eq!(render_with_opts(&doc, false, RenderOpts::ON), "");
        // 无页可标记，但函数不崩；`total_pages` 的 `map_or(1, ..)` 保证非 0。
        // （无可断言的输出——故这条只钉住「不 panic」。）
    }

    /// 从输出里抽出全部 `(页号, 总数)` 对，供不变量断言用。
    fn page_numbers(out: &str) -> Vec<(u32, u32)> {
        out.match_indices("<!-- page ")
            .filter_map(|(i, _)| {
                let body = out[i + "<!-- page ".len()..].split("-->").next()?;
                let (n, m) = body.split_once(" of ")?;
                Some((n.trim().parse().ok()?, m.trim().parse().ok()?))
            })
            .collect()
    }

    /// 开关关闭：逐字节回到 #9 之前的行为（空段跳过、无标记）。
    #[test]
    fn page_marker_off_restores_legacy_output() {
        let doc = DocIR {
            pages: vec![
                page_at(0, PageSource::Ocr, vec![Region::new(0.0, 1.0, 0.0, 1.0, "甲")]),
                page_at(1, PageSource::Ocr, vec![]),
                page_at(2, PageSource::Ocr, vec![Region::new(0.0, 1.0, 0.0, 1.0, "乙")]),
            ],
        };
        assert_eq!(render_with_opts(&doc, false, RenderOpts::OFF), "甲\n\n乙");
        // ON 档：空页的标记把两段分开。
        assert_eq!(
            render_with_opts(&doc, false, RenderOpts::ON),
            "<!-- page 1 of 3 -->\n\n甲\n\n<!-- page 2 of 3 -->\n\n<!-- page 3 of 3 -->\n\n乙"
        );
    }

    /// 三个开关组合的**交叉一致性**：`page_marker` 与 `image_marker` 互不干扰
    /// （分页标记塞在段首、图片占位塞在段内body 流）。
    #[test]
    fn page_and_image_markers_are_independent() {
        let img = Region::new(0.0, 1.0, 0.0, 1.0, "图内文字").with_kind(RegionKind::Image);
        let mk = |pm: bool, im: bool| {
            let doc = DocIR {
                pages: vec![page_at(0, PageSource::Ocr, vec![
                    Region::new(0.0, 1.0, 0.0, 1.0, "标题"),
                    img.clone(),
                ])],
            };
            render_with_opts(&doc, false, RenderOpts { page_marker: pm, image_marker: im })
        };
        assert_eq!(mk(true, false), "<!-- page 1 of 1 -->\n\n标题\n图内文字");
        assert_eq!(mk(false, true), "标题\n\n<!-- image page:1 -->");
        assert_eq!(mk(true, true), "<!-- page 1 of 1 -->\n\n标题\n\n<!-- image page:1 -->");
        assert_eq!(mk(false, false), "标题\n图内文字");
    }

    // ==================== #9 gap-B：图片块占位 ====================

    /// 图片占位形态 `<!-- image page:N -->`，**页号 1 基**，块内文字不进正文流。
    #[test]
    fn image_placeholder_carries_one_based_page_and_drops_inner_text() {
        let doc = DocIR {
            pages: vec![
                page_at(0, PageSource::Ocr, vec![Region::new(0.0, 1.0, 0.0, 1.0, "前置正文")]),
                page_at(6, PageSource::Ocr, vec![
                    Region::new(0.0, 1.0, 0.0, 1.0, "起点终点输入源")
                        .with_kind(RegionKind::Image),
                    Region::new(0.0, 1.0, 1.0, 2.0, "输入活动输出")
                        .with_kind(RegionKind::Image),
                ]),
            ],
        };
        let out = render_with_opts(&doc, false, RenderOpts::ON);
        assert_eq!(
            out,
            "<!-- page 1 of 7 -->\n\n前置正文\n\n<!-- page 7 of 7 -->\n\n<!-- image page:7 -->"
        );
        assert!(!out.contains("起点终点"), "图内文字不应进正文流：{out}");
    }

    /// **相邻连续的 Image 行压成一个**（一个版面 Image 元素 bbox 内 N 行
    /// →一个占位），与 `collapse_chart_runs` 同构。
    #[test]
    fn consecutive_image_lines_collapse_to_one_placeholder() {
        let doc = DocIR {
            pages: vec![page_at(0, PageSource::Ocr, vec![
                Region::new(0.0, 1.0, 0.0, 1.0, "a").with_kind(RegionKind::Image),
                Region::new(0.0, 1.0, 1.0, 2.0, "b").with_kind(RegionKind::Image),
                Region::new(0.0, 1.0, 2.0, 3.0, "c").with_kind(RegionKind::Image),
            ])],
        };
        assert_eq!(
            render_with_opts(&doc, false, RenderOpts::ON),
            "<!-- page 1 of 1 -->\n\n<!-- image page:1 -->"
        );
    }

    /// 两张图被图注隔开 → **两个**占位（「两张图」的正确语义，与 Chart 同构）。
    #[test]
    fn two_images_separated_by_caption_render_two_placeholders() {
        let doc = DocIR {
            pages: vec![page_at(0, PageSource::Ocr, vec![
                Region::new(0.0, 1.0, 0.0, 1.0, "a").with_kind(RegionKind::Image),
                Region::new(0.0, 1.0, 1.0, 2.0, "图 1 标题"),
                Region::new(0.0, 1.0, 2.0, 3.0, "b").with_kind(RegionKind::Image),
            ])],
        };
        assert_eq!(
            render_with_opts(&doc, false, RenderOpts::ON),
            "<!-- page 1 of 1 -->\n\n<!-- image page:1 -->\n\n图 1 标题\n\n<!-- image page:1 -->"
        );
    }

    /// 开关关闭：图变回普通正文行，**图内文字不丢**。
    ///
    /// 注意与 Chart 的差异：Chart 恒丢弃块内文字（轴标签/图例在正文里没意义），
    /// Image 的块内文字可能是正文读者需要的，所以关掉占位只关标记、不关内容。
    #[test]
    fn image_marker_off_drops_whole_block() {
        let doc = DocIR {
            pages: vec![page_at(0, PageSource::Ocr, vec![
                Region::new(0.0, 1.0, 0.0, 1.0, "前置正文"),
                Region::new(0.0, 1.0, 1.0, 2.0, "a").with_kind(RegionKind::Image),
                Region::new(0.0, 1.0, 2.0, 3.0, "b").with_kind(RegionKind::Image),
                Region::new(0.0, 1.0, 3.0, 4.0, "后置正文"),
            ])],
        };
                // OFF：图变回普通正文行，夹在两段正文之间（单换行，与 Chart 恒丢弃
        // 不同——Image 的块内文字对读者有意义）。
        assert_eq!(render_with_opts(&doc, false, RenderOpts::OFF), "前置正文\na\nb\n后置正文");
    }

    /// 开关关闭时，图中文字回到正文流，且**两个 chart 都要在**。
    ///
    /// 这条是 `collapse_runs` 状态机 bug 的回归钉子：初版用
    /// `prev_was_chart` + `in_image` 两个 bool，Image 分支的早退 `continue`
    /// 漏了写 `prev_was_chart` → 序列 `Chart, Image, Chart` 里第二个 Chart
    /// 被误当「同一张图的续行」丢掉。
    #[test]
    fn chart_image_chart_sequence_keeps_both_charts() {
        let doc = DocIR {
            pages: vec![page_at(0, PageSource::Ocr, vec![
                Region::new(0.0, 1.0, 0.0, 1.0, "c1").with_kind(RegionKind::Chart),
                Region::new(0.0, 1.0, 1.0, 2.0, "i1").with_kind(RegionKind::Image),
                Region::new(0.0, 1.0, 2.0, 3.0, "c2").with_kind(RegionKind::Chart),
            ])],
        };
        // 开启：两个 chart 占位 + 一个 image 占位，三个都在。
        assert_eq!(
            render_with_opts(&doc, false, RenderOpts::ON),
            "<!-- page 1 of 1 -->\n\n<!-- chart -->\n\n<!-- image page:1 -->\n\n<!-- chart -->"
        );
        // 关闭：中间那张图变回普通正文行（`i1`），**两个 chart 都要在**。
        assert_eq!(
            render_with_opts(&doc, false, RenderOpts::OFF),
            "<!-- chart -->\n\ni1\n\n<!-- chart -->"
        );
    }

    /// `Chart, Image(连续 2 行), Chart` 在 Image **开启**时同样不能吃掉尾随
    /// Chart（`prev` 状态在 Image 续行 `continue` 时也必须写）。
    #[test]
    fn image_continuation_line_does_not_swallow_following_chart() {
        let doc = DocIR {
            pages: vec![page_at(0, PageSource::Ocr, vec![
                Region::new(0.0, 1.0, 0.0, 1.0, "c1").with_kind(RegionKind::Chart),
                Region::new(0.0, 1.0, 1.0, 2.0, "i1").with_kind(RegionKind::Image),
                Region::new(0.0, 1.0, 2.0, 3.0, "i2").with_kind(RegionKind::Image),
                Region::new(0.0, 1.0, 3.0, 4.0, "c2").with_kind(RegionKind::Chart),
            ])],
        };
        assert_eq!(
            render_with_opts(&doc, false, RenderOpts::ON),
            "<!-- page 1 of 1 -->\n\n<!-- chart -->\n\n<!-- image page:1 -->\n\n<!-- chart -->"
        );
    }

    /// 图片占位是独立注释块，**前后各留一个空行**——否则会被并进相邻正文段
    /// （与 `CHART_PLACEHOLDER` 同口径）。
    #[test]
    fn image_placeholder_has_blank_line_around_it() {
        let doc = DocIR {
            pages: vec![page_at(0, PageSource::Ocr, vec![
                Region::new(0.0, 1.0, 0.0, 1.0, "前置正文"),
                Region::new(0.0, 1.0, 1.0, 2.0, "图内").with_kind(RegionKind::Image),
                Region::new(0.0, 1.0, 2.0, 3.0, "后置正文"),
            ])],
        };
        let out = render_with_opts(&doc, false, RenderOpts::ON);
        assert_eq!(
            out,
            "<!-- page 1 of 1 -->\n\n前置正文\n\n<!-- image page:1 -->\n\n后置正文",
            "got: {out:?}"
        );
        // 占位独占一行：其前后都是空行（\n\n）。
        assert!(out.contains("\n\n<!-- image page:1 -->\n\n"));
    }
}
