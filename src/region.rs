//! 文本区域统一类型：替代全仓贯穿的 `(f32, f32, f32, f32, String)` 元组。
//!
//! 字段语义（与 `reading_order` 约定一致）：`x_min`/`x_max` 为区域水平范围，
//! `y_min`/`y_max` 为垂直范围（**越小越靠上**——PDF 调用侧已翻转、OFD 原生左上），
//! `text` 为区域文本。表网格通路接收的 `(x, y, w, h, text)` 块用
//! [`Region::from_top_left`] 转换后存入同一类型（`h = y_max - y_min`）。
//!
//! P1.5 DocIR：Region 扩展 [`kind`](RegionKind)（版面语义）与
//! [`confidence`](Region::confidence)（OCR 识别置信度；文字层源恒 `None`），
//! 成为三源统一的版面级区块载体。
//!
//! #6 第 2 步：再加 [`heading_level`](Region::heading_level)。标题从此是
//! **数据**——producer 只赋级别，`#` 前缀由 `docir/render.rs` 写出，投影层
//! （content_list / middle_json，#10/#11）直接读级别而不用反解 markdown 字面量。
//!
//! #6 第 4 步：再加 [`Span`] + [`Region::spans`]。行内样式从此也是**数据**——
//! PDF 文字层从 pdf-inspector 的样式证据（`is_bold`/`is_italic`/几何装饰/
//! `baseline_shift`）直接产 spans；OFD 文字层与 OCR 通路无样式证据，产"单
//! span、全零样式"的退化形态（行边界仍然结构化）。`spans` 是**旁路信息**：
//! markdown 渲染层不消费它（输出逐字节不变），投影层（#11）直读。
//!
//! 整宽判定阈值集中于此，消除 `reading_order` 内 0.92/0.08 的重复魔法数。

use crate::table_grid::TableGrid;

/// 整宽判定：区域跨度须 > 此比例 × 页宽（剔除通栏正文长条）。
pub const FULL_WIDTH_THRESHOLD: f32 = 0.92;
/// 整宽判定：区域左缘须 < 此比例 × 页宽（贴近左页边）。
pub const EDGE_MARGIN: f32 = 0.08;

/// markdown 标题级别上限（与 [`crate::heading_levels::LEVEL_MAX`]、`title_level`
/// 的 clamp 同口径）。#6 第 2 步起 `Region.heading_level` 用它做上界。
pub const HEADING_LEVEL_MAX: usize = 6;

/// 区块版面语义（P1.5）：标注 Region 在 DocIR 装配/后处理中的角色。
/// 渲染层按 kind 分流（正文行/表格 HTML/网格表/成品块），不依赖来源类型。
///
/// #6 第 5 步扩容：`Image`/`Code`/`Formula`/`Index`/`Aside` 五类**枚举先行、
/// producer 未产**——它们的出现依赖 #10 的块类型样本（chart → code → list →
/// index/aside/footnote 拆小票），届时才有对应渲染分支（fence/`$$`/裁图落盘）。
/// 在那之前渲染层对它们零消费（与 `Body` 之外的既有类别一样不输出），
/// 枚举先行的意义是让 #10 的投影层与单测有**可断言的落点**。
///
/// **#10 补全后的现状**：`Code`/`Index`/`Aside`/`Reference`/`Chart` 已有
/// producer 与渲染分支；`Image`/`Formula` **仍是零消费占位**（`Image` 的未来
/// 行为是裁图落盘 + `![](images/…)`，与本仓 `Chart` 的"注释占位、不产资产"
/// 是两条不同的线，勿混）。
///
/// `Footnote` 与 `Noise` 本步就有 producer（#10 例外项）：OCR 通路的
/// `Footnote` 版面元素与页面家具（页眉/页脚/页码/印章区）不再静默丢弃，
/// 而是以独立 kind 进 IR；渲染层**默认跳过**（输出逐字节不变），开关
/// `ANYDOC_EMIT_FURNITURE` 打开时以 HTML 注释行输出（见 `docir/render.rs`）。
#[derive(Clone, Debug, PartialEq)]
pub enum RegionKind {
    /// 正文文本行：producer 已完成阅读顺序还原与**标题级别赋值**（`text` 是
    /// 未加 `#` 前缀的最终行文本，前缀由渲染器按 [`Region::heading_level`] 写出）。
    Body,
    /// 网格重建表（文字层网格 / OCR Image 块补救）：跨页表合并 pass 的对象。
    Grid(TableGrid),
    /// OCR 识别表：`text` = 已 simplify 的 `<table>…</table>` HTML。
    TableHtml,
    /// 已渲染块：`text` = producer 产出的成品 markdown 片段（**含精确分隔符**，
    /// 渲染层原样追加，不二次加工——保证与旧 emitter 通路字节一致）。
    PreRendered,
    /// 图片块（#6 第 5 步占位）：`text` 暂存 OCR 读出的块内文字（若有）。
    /// 未来行为（依赖 #10）：从渲染位图裁切落盘 + `![](images/…)` 引用，
    /// 块内文字不进正文流（对齐 MinerU basic 的 image 块处理）。
    #[allow(dead_code)] // 占位变体：producer 未产（依赖 #10 样本），消费方是 #10 渲染分支
    Image,
    /// 图表块（#10 chart 票，2026-10-03）：版面 `Chart` 元素 bbox 内的行回贴
    /// 本 kind（`gfm_adapter::mark_layout_kinds`，与 `Aside`/`Code` 同构）。
    ///
    /// **markdown 端丢弃块内文字**：`text` 暂存 OCR 读出的图内文字（轴标签/
    /// 图例/数据标签），渲染层**不输出**它——只写一个 `<!-- chart -->` HTML
    /// 注释占位。本仓不产图片资产，写 `![](…)` 是死链，故用注释占位。
    ///
    /// 与 MinerU basic 档的差别是**有意的**（勿"对齐"回去）：basic 把图内文字
    /// 100% 丢弃（`ChartBodyBlock(ImagePayloadContentBlock)` 的
    /// `content: str` 是类型层面的强制丢弃，为 VLM 二次填充预留，
    /// `postprocess/page_blocks.py:90-91` 置空串），本仓 OCR 已经拿到这些文字，
    /// 照抄即是无谓的信息损失。改用 HTML 注释是取"两头都要"：不把图内文字
    /// 塞进正文流（对齐 basic 的可读性），又让"此处有图被有意略过"这件事
    /// **可观测**（下游能区分"有图被略过"与"文档本来没图"）。
    ///
    /// **图注不靠本 kind 承载**：图注是**独立的 `FigureTitle` 版面元素**
    /// （synth_samples.pdf 第1 页实测：chart 元素 `text` 的 15 行全是图内数据，
    /// 无一行是图注；图注是同页两个 `figure_title` 元素，y=340-358 与
    /// y=665-682，分别在 chart bbox `[358,642]` 的上方与下方）。图注照常走
    /// 普通正文流（Body），因此本 kind 无需从 `text` 里剥离图注。
    ///
    /// 渲染层把**相邻连续**的 Chart 行聚合成**一个** `<!-- chart -->`（同一张图
    /// 只留一个占位，对齐 MinerU「一个 ChartBlock 一个块」），位置在阅读序原处
    /// ——图注因此自然落在占位前后。
    Chart,
    /// 代码块（#10 补全，2026-10-01）：版面 `Algorithm` 元素 bbox 内的行回贴
    /// 本 kind。渲染为 fenced code block（连续 Code 行共享一个围栏）。
    #[allow(dead_code)] // 同上
    Code,
    /// 独立公式块（#6 第 5 步占位）：producer 未产。未来渲染为 `$$…$$`。
    /// （行内公式 `SpanKind::Equation` 的落点在 span 层，见 BACKLOG #9/#6 第 0 步。）
    #[allow(dead_code)] // 同上
    Formula,
    /// 目录块（MinerU 13 项之 `INDEX`）：**有 producer**（#10 INDEX 票，2026-09-30）。
    /// 一条 Region = 一条目次条目（含点线引导符的那类），渲染为 `- ` 列表项，
    /// content_list v2 由相邻连续条目聚合成**一个** `index` item。
    /// 文字层专用信号：点线形态是文字层无版面模型时唯一拿得到的 INDEX 证据，
    /// OCR 通路暂不产（它的正途是接版面模型的 `IndexBlock`）。
    Index,
    /// 旁注/边注（#6 第 5 步占位）：producer 未产。MinerU 13 项之 `ASIDE_TEXT`。
    ///
    /// #10 补全（2026-10-01）：OCR 通路已产——版面 `AsideText` 元素 bbox 内的
    /// 行在装配后回贴本 kind（`gfm_adapter::mark_layout_kind`）。渲染层按
    /// **普通正文段**输出（MinerU `PageAuxTextBlock` 的 markdown 形态就是无
    /// 标记段落，`docvortex blocks.py::PageAuxTextBlock` 分支），content_list
    /// v2 投影为独立 `page_aside_text` item（`ct::PAGE_ASIDE_TEXT`）。
    Aside,
    /// 参考文献条目（#10 补全，2026-10-01）：版面 `Reference`/`ReferenceContent`
    /// 元素 bbox 内的行回贴本 kind。渲染层按**普通正文段**输出（MinerU
    /// `RefTextBlock` 的 markdown 形态就是无标记段落）；content_list v2 由
    /// 相邻连续条目聚合成一个 `{"type":"list","list_type":"reference_list"}`
    /// item（MinerU `v2.py::_reference_list_item`，无 `attribute`——与
    /// `text_list` 的差别）。
    Reference,
    /// 脚注（#10 例外项，本步有 producer）：OCR 通路 `Footnote` 版面元素内的
    /// 文本行。**注意**这不是"被页脚吸收"——`is_footer()` 把 `Footnote` 与
    /// Footer 并列是 oar-ocr 的类型划分口径；MinerU 13 项里 `PAGE_FOOTNOTE`
    /// 是独立类型（content_list v2 有独立 item），故此处独立成 kind 而非
    /// 并入 [`RegionKind::Noise`]。渲染默认跳过，开关打开时输出。
    Footnote,
    /// 页面家具（#10 例外项，本步有 producer）：页眉/页脚/页码/印章区文本。
    /// 此前 OCR 通路对它们三重丢弃（收集层 `continue` + 阅读序跳过 + leftover
    /// 排除），现改"收集进 IR、渲染默认跳过"——信息不再无痕丢失，投影层
    /// （#10 content_list v2）可落 `PAGE_HEADER`/`PAGE_FOOTER`/`PAGE_NUMBER`
    /// 独立 item（MinerU 同语义：`NOT_EXTRACT_TYPES` 不进提取、但 v2 有类型）。
    Noise(NoiseKind),
}

/// 页面家具细分（#10 例外项）。与 OCR 版面元素的对应：
/// `Header`/`HeaderImage` → [`NoiseKind::Header`]；`Footer`/`FooterImage` →
/// [`NoiseKind::Footer`]；`Number` → [`NoiseKind::PageNumber`]；
/// `Seal` → [`NoiseKind::Seal`]。`Footnote` 不在此列（独立
/// [`RegionKind::Footnote`]，见其文档）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoiseKind {
    Header,
    Footer,
    PageNumber,
    /// 印章区散文本（det 读出的章内文字）。与 `seal_pass` 的 `【印章】` 行
    /// 是两条通路：后者来自印章检测+识别的专门 pass（#10b），前者是版面
    /// `Seal` 元素 bbox 内的散落 OCR 文本。默认都不进正文。
    Seal,
}

/// 行内样式位（#6 第 4 步）。字段名与 MinerU 严格 schema 的
/// `TextSpan.styles` 集合对齐（`docvortex schema.py:320-338`：
/// bold/italic/underline/emphasis/strikethrough/superscript/subscript），
/// 差异两处，都是**信息只多不少**的方向：
/// - `emphasis` 本仓无证据来源（pdf-inspector 无对应判定），第一版不设位；
/// - MinerU 校验 superscript/subscript 互斥（`schema.py:341-356`），本仓两个
///   位独立存放（同一 run 不可能同时非零 `baseline_shift`，实际上也互斥），
///   互斥决策留给投影层，不在 IR 层丢信息。
///
/// 另注意 pdf-inspector 的装饰判定口径：`<u>`/`<s>` 是**几何检测**（画出来的
/// 线），且它自己的 markdown 渲染里装饰与字体样式互斥（strike > underline >
/// bold/italic）。span 层保留独立位（证据原样），互斥折叠是投影层的事。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpanStyles {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub superscript: bool,
    pub subscript: bool,
}

impl SpanStyles {
    /// 全零样式（绝大多数正文 run）。
    pub fn is_plain(&self) -> bool {
        !(self.bold
            || self.italic
            || self.underline
            || self.strikethrough
            || self.superscript
            || self.subscript)
    }
}

/// 行内 span（#6 第 4 步）：一段样式连续的 run 文本。
///
/// **与 [`Region::text`] 的分工**（两层真相，各有管辖）：
/// - `text` 是渲染/判定的真相——由 `TextLine::text()`（text_plain）产出，
///   **含** `<sup>…</sup>`/`<sub>…</sub>` 字面标签与跨 item 插入空格；
/// - `spans[].text` 是结构化的 run 文本——item 原文原样拼接，**不含**标签
///   （上下标在 [`SpanStyles`] 位上），跨 span 的插入空格只做简化几何判定
///   （见 `pdf/text_layer.rs::build_spans`，边缘形态不与 text_plain 复刻对齐）。
///
/// 因此**不存在**"spans 拼接 == text"的逐字节不变式；两层的强一致校验是
/// "剥掉标签与空白后内容相等"，由 producer 侧单测钉住。markdown 渲染层
/// 不消费 spans（输出逐字节不变），投影层（#11）直读。
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub text: String,
    pub styles: SpanStyles,
}

impl Span {
    pub fn new(text: impl Into<String>, styles: SpanStyles) -> Self {
        Span {
            text: text.into(),
            styles,
        }
    }

    /// 全零样式的 span（OFD / OCR 等无样式证据来源的退化形态）。
    pub fn plain(text: impl Into<String>) -> Self {
        Span {
            text: text.into(),
            styles: SpanStyles::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    pub x_min: f32,
    pub x_max: f32,
    pub y_min: f32,
    pub y_max: f32,
    pub text: String,
    /// 版面语义（P1.5）：构造函数默认 [`RegionKind::Body`]。
    pub kind: RegionKind,
    /// 识别置信度（P1.5）：OCR 源为 `Some(score)`；文字层源无此概念（`None`）。
    pub confidence: Option<f32>,
    /// 行内 span（#6 第 4 步）：样式证据的结构化载体。空 vec = 该来源无样式
    /// 信息（OFD 文字层 / OCR 通路退化为单 span 或干脆空），渲染层不消费。
    pub spans: Vec<Span>,
    /// 标题级别（#6 第 2 步）：`Some(1..=6)` 表示该行为标题行。`#` 前缀**不在
    /// `text` 里**，由 [`Region::rendered_line`] 在渲染时写出——级别从此是 IR
    /// 数据，投影层（#10/#11）直接读它，不必反解 markdown 字面量。
    ///
    /// 来源文本自带 `#` 字面量时（markdown 被印进 PDF/OFD 文字层、OCR 读到
    /// `#` 开头的行）级别由字面量的 `#` 段数解析，`rendered_line` 据此
    /// **不重复写前缀**。这是旧 `apply_title_prefixes` "防双重标记"规则的原样
    /// 搬迁，不是新语义。
    pub heading_level: Option<u8>,
    /// 跨页续接标记（#6 第 3 步）：`Some(true)` = **本区块的内容已并入前面某页
    /// 的同列网格表**，自身只作占位保留（对齐 MinerU `continues_prev: bool | None`，
    /// `schema.py:506-509`/`:707`，仅顶层块携带，嵌套块禁止 `schema.py:1057-1058`）。
    ///
    /// 由 `docir/passes/cross_page_table` 写入，producer 恒 `None`。与 MinerU 的
    /// **内容口径差别**要在投影层（#10/#11）注意：MinerU 里带标记的块自己仍带正文
    /// （合并发生在更后置的通路），本仓的合并发生在**这个 pass 里**——首表页那份是
    /// 合并结果，续页标记块保留的是 producer 原始 grid（未去重、未并入）。两者内容
    /// 不会重复输出：渲染层按 [`Region::is_continues_prev`] 跳过标记块，这条与
    /// "删除区块"的旧形状渲染逐字节相同（单测
    /// `absorbed_stub_renders_identically_to_deletion`）。投影层若把标记块当正文输出
    /// 就会**重行**，必须同样跳过或只取 `continues_prev` 这个事实。
    ///
    /// 其余取值：`None` = 非续接块（绝大多数区块，含每页表格的首块）；
    /// `Some(false)` = 显式"不是续接"，与 `None` 渲染行为相同，仅供投影层显式
    /// 落 `false` 时使用（MinerU 允许三态）。
    pub continues_prev: Option<bool>,
    /// 行字号（#11c-v3）：该行/段内最大 em 高度（pt，PDF `TextItem.font_size`；
    /// OFD `TextObject@Size` 毫米原值——**只在文档内做相对比值，量纲无关**）。
    ///
    /// `None` = 来源无字号证据（OCR det 框、表格/占位构造、旧调用方默认）。
    /// MinerU 4.0.8 文字层 span 模型**不携带字号**（段落真值=版面框）；本仓
    /// 无版面模型，字号是"模拟 block 边界"护栏（`merge_into_paragraphs`）的
    /// 判据来源——真实语料区分度实证见 BACKLOG #11c-v3（GJB：正文 10pt vs
    /// 无编号标题 16/26pt）。渲染层不消费，投影层（#10/#11）可直读。
    pub font_size: Option<f32>,
    /// 列表项标注（#10 切片 4）：本段以列表 marker 开头（`starts_with_list_marker`
    /// 判定，与切片 1 的 merge 独段护栏**同判据、同文本视图**；标题行不打——
    /// 赋了级别的行不是列表项）。**渲染层不感知**：MinerU 对 text_list 的
    /// markdown 形态就是条目原文逐行输出（`markdown/blocks.py::_render_list`
    /// 保留原 marker，不换 `- `），与普通段落无 markdown 差别 → 输出零变化。
    /// 唯一消费方是 content_list v2 投影（`docir/content_list.rs`）：相邻连续
    /// 的 list_item 聚合成**一个** `list` item（v2.py `_render_list`：
    /// `list_type: text_list` + 逐条 `list_items` + `attribute`）。
    ///
    /// MinerU 口径备注：basic 档**不产** ListBlock（版面 23 类无 list 标签、
    /// `PIPELINE_DET_TYPE` 不含 LIST）——列表结构是 VLM 线产物。本仓的
    /// marker 检测（切片 1/2）是对该缺口的独立增强，投影形态对齐 VLM 线。
    pub list_item: bool,
    /// 目录块成员（#10 切片 5 · OCR 通路）：本行落在版面 Content 块
    /// （PP-DocLayout-S 类别 5 "content"，MinerU `VLM_LAYOUT_LABEL_MAP` →
    /// `BlockType.INDEX`）内，**且**该块被确证是目录块（块内存在点线引导行）。
    ///
    /// 两个用途，都是"行级"而非"块级"：
    /// 1. **段落合并围栏**：`merge_into_paragraphs` 对带此标记的行双向开新段
    ///    → 目录页逐条独立，不再整页并成一坨（OCR 丢点线的行 `1 范围1`
    ///    `7 支持5` 靠形态判据接不住，只能靠版面几何）。
    /// 2. **INDEX 回贴**：`mark_layout_index` 把它当"已在目录块内"的证据，
    ///    不必要求行自身含点线。
    ///
    /// 渲染层不感知（与 `list_item` 同）。
    pub index_member: bool,
}

impl Region {
    pub fn new(x_min: f32, x_max: f32, y_min: f32, y_max: f32, text: impl Into<String>) -> Self {
        Region {
            x_min,
            x_max,
            y_min,
            y_max,
            text: text.into(),
            kind: RegionKind::Body,
            confidence: None,
            spans: Vec::new(),
            heading_level: None,
            continues_prev: None,
            font_size: None,
            list_item: false,
            index_member: false,
        }
    }

    /// 从左上角 + 宽高构造（表网格块 `(x, y, w, h)` 形式）。
    pub fn from_top_left(x: f32, y: f32, w: f32, h: f32, text: impl Into<String>) -> Self {
        Region {
            x_min: x,
            x_max: x + w,
            y_min: y,
            y_max: y + h,
            text: text.into(),
            kind: RegionKind::Body,
            confidence: None,
            spans: Vec::new(),
            heading_level: None,
            continues_prev: None,
            font_size: None,
            list_item: false,
            index_member: false,
        }
    }

    /// 附加版面语义（builder）。
    pub fn with_kind(mut self, kind: RegionKind) -> Self {
        self.kind = kind;
        self
    }

    /// 附加识别置信度（builder，OCR 源）。
    pub fn with_confidence(mut self, confidence: Option<f32>) -> Self {
        self.confidence = confidence;
        self
    }

    /// 附加标题级别（builder，#6 第 2 步）。
    pub fn with_heading_level(mut self, level: Option<u8>) -> Self {
        self.heading_level = level;
        self
    }

    /// 附加行内 span（builder，#6 第 4 步）。
    pub fn with_spans(mut self, spans: Vec<Span>) -> Self {
        self.spans = spans;
        self
    }

    /// 附加行字号（builder，#11c-v3；`None` = 无字号证据）。
    pub fn with_font_size(mut self, font_size: Option<f32>) -> Self {
        self.font_size = font_size;
        self
    }

    /// 标注为目录块成员（builder，#10 切片 5）。
    pub fn with_index_member(mut self, index_member: bool) -> Self {
        self.index_member = index_member;
        self
    }

    /// 是否为"内容已并入前页表格"的占位块（#6 第 3 步）。
    ///
    /// 渲染层据此**跳过**该块（输出与旧通路"物理删除续页区块"逐字节相同）；
    /// 投影层（#10）据此在续页上落 `continues_prev: true` 的块而不是"表格消失"。
    /// 只认 `Some(true)`：`None`/`Some(false)` 均按普通块渲染。
    pub fn is_continues_prev(&self) -> bool {
        self.continues_prev == Some(true)
    }

    /// 文本**开头**连续 `#` 的级数，无则 `None`；超过 [`HEADING_LEVEL_MAX`] 按上限计。
    ///
    /// 不做 trim：视图由调用方决定（判定视图用 `trim_start()` 后的文本，与旧
    /// `apply_title_prefixes` 规则 1 同口径）。用途：来源文本自带 markdown
    /// 字面量的行（markdown 被印进 PDF/OFD 文字层、OCR 读到 `#` 开头的行），
    /// 级别由字面量给出，渲染时 `rendered_line` 不重复写前缀。
    pub fn leading_hash_level(text: &str) -> Option<u8> {
        let run = text.chars().take_while(|c| *c == '#').count();
        (run > 0).then(|| run.min(HEADING_LEVEL_MAX as usize) as u8)
    }

    /// 该行的**渲染文本**（#6 第 2 步：`#` 前缀在此写出，不进 IR）。
    ///
    /// `heading_level = Some(lv)` 且判定视图不带字面量前缀 →
    /// `"#".repeat(lv) + " " + text`；其余（正文行、或文本已自带 `#` 字面量）→
    /// 原样。与旧 `text_health::apply_title_prefixes` 的输出逐字节等价。
    pub fn rendered_line(&self) -> std::borrow::Cow<'_, str> {
        match self.heading_level {
            Some(lv) if !self.text.trim_start().starts_with('#') => {
                format!("{} {}", "#".repeat(usize::from(lv)), self.text).into()
            }
            _ => std::borrow::Cow::Borrowed(&self.text),
        }
    }

    /// 标题行判定（**渲染视图**）：渲染后的行字面以 `#` 开头，**不 trim**。
    /// 与旧 `docir/render.rs` 里 `t.starts_with('#')`（空行语义的依据）逐字节等价
    /// ——含"`  # 字面量`"（前导空白 + `#`）这种形态在两处都判为非标题。
    pub fn is_heading(&self) -> bool {
        self.rendered_line().starts_with('#')
    }

    /// 标题行判定（**判定视图**）：渲染后行 `trim_start()` 再以 `#` 开头。
    /// 对齐旧代码里 `line.trim_start().starts_with('#')`（列表配对跨项判别）与
    /// `line.trim().starts_with('#')`（噪声碎片剔除）两处口径——它们都带 trim，
    /// 故与 [`is_heading`](Self::is_heading) 的差别只在"前导空白 + `#`"一种形态。
    pub fn is_heading_trimmed(&self) -> bool {
        self.rendered_line().trim_start().starts_with('#')
    }

    pub fn width(&self) -> f32 {
        self.x_max - self.x_min
    }

    pub fn height(&self) -> f32 {
        self.y_max - self.y_min
    }

    /// 本块是否带**真实几何**（非零面积的正框）。
    ///
    /// 多个 producer 用 `Region::new(0,0,0,0, text)` 造"只有文本没有框"的块
    /// （OFD 文字层正文、PDF 文字层成品表格 / OCR 兜底页、`gfm_adapter` 的
    /// 表/网格块……）。渲染层不吃几何所以无人关心，但 #11 的 bbox 投影吃——
    /// 把退化框当真就会输出 `[0,0,0,0]`，那是**造数据**（见 #6 第 1 步
    /// "不伪造分母"同一条纪律）。故投影层遇退化框必须省略 `bbox` 键。
    pub fn has_geometry(&self) -> bool {
        self.width() > 0.0 && self.height() > 0.0
    }

    pub fn center_x(&self) -> f32 {
        (self.x_min + self.x_max) / 2.0
    }

    /// 整宽判定：跨度 > 92% 页宽 且 左缘 < 8% 页宽 → 页眉/页脚/通栏标题。
    /// 与 `detect_column_split` 共用同一口径（同一常量）。
    pub fn is_full_width(&self, page_w: f32) -> bool {
        self.width() > FULL_WIDTH_THRESHOLD * page_w && self.x_min < EDGE_MARGIN * page_w
    }

    /// 一组区域的最大 `x_max`（页宽估计）。
    pub fn page_w(regions: &[Region]) -> f32 {
        regions.iter().map(|r| r.x_max).fold(0.0_f32, f32::max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_top_left_expands_to_box() {
        let r = Region::from_top_left(10.0, 20.0, 30.0, 40.0, "x");
        assert_eq!(r.x_min, 10.0);
        assert_eq!(r.x_max, 40.0);
        assert_eq!(r.y_min, 20.0);
        assert_eq!(r.y_max, 60.0);
        assert_eq!(r.width(), 30.0);
        assert_eq!(r.height(), 40.0);
    }

    #[test]
    fn is_full_width_matches_threshold() {
        // 页宽 1000：整宽须跨度 >920 且 左缘 <80
        assert!(Region::new(0.0, 1000.0, 5.0, 15.0, "hdr").is_full_width(1000.0));
        // 左列长条目：跨度 400（<920）→ 非整宽
        assert!(!Region::new(50.0, 450.0, 100.0, 110.0, "L").is_full_width(1000.0));
        // 左缘贴边但跨度不足 → 非整宽
        assert!(!Region::new(0.0, 800.0, 100.0, 110.0, "M").is_full_width(1000.0));
        // 跨度够但左缘不贴边 → 非整宽
        assert!(!Region::new(100.0, 1020.0, 100.0, 110.0, "R").is_full_width(1000.0));
    }

    #[test]
    fn page_w_is_max_x_max() {
        let rs = vec![
            Region::new(0.0, 500.0, 0.0, 10.0, "a"),
            Region::new(0.0, 800.0, 0.0, 10.0, "b"),
            Region::new(0.0, 300.0, 0.0, 10.0, "c"),
        ];
        assert_eq!(Region::page_w(&rs), 800.0);
    }

    // ---- #6 第 4 步：span 断言 ----

    #[test]
    fn region_defaults_to_no_spans() {
        let r = Region::new(0.0, 10.0, 0.0, 10.0, "x");
        assert!(r.spans.is_empty());
        let r = Region::from_top_left(0.0, 0.0, 10.0, 10.0, "x");
        assert!(r.spans.is_empty());
    }

    #[test]
    fn span_styles_plain_and_bits() {
        let plain = SpanStyles::default();
        assert!(plain.is_plain());
        let bold = SpanStyles {
            bold: true,
            ..SpanStyles::default()
        };
        assert!(!bold.is_plain());
    }

    #[test]
    fn with_spans_attaches_and_keeps_text_independent() {
        // spans 是旁路信息：text 不由 spans 派生，二者独立存在。
        let r = Region::new(0.0, 10.0, 0.0, 10.0, "plain truth")
            .with_spans(vec![Span::new(
                "plain truth",
                SpanStyles {
                    bold: true,
                    ..SpanStyles::default()
                },
            )]);
        assert_eq!(r.text, "plain truth");
        assert_eq!(r.spans.len(), 1);
        assert!(r.spans[0].styles.bold);
        assert_eq!(r.spans[0].text, "plain truth");
    }

    #[test]
    fn span_plain_helper_is_zero_styles() {
        let s = Span::plain("行文本");
        assert!(s.styles.is_plain());
        assert_eq!(s.text, "行文本");
    }
}
