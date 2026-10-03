//! #11 结构化输出投影：content_list **v2** 优先。
//!
//! 口径来源（本机 MinerU 4.0.5 实测源码，勿凭记忆改动）：
//! - 顶层形状：`list[list[dict]]`——**按页分组**的数组，每 item `{type, content, bbox}`
//!   （`render/_internal/content_list/v2.py::render_content_list_v2`）。
//! - bbox：`0–1000` 归一化**整数**，`normalize_bbox` = `int(v * 1000)`
//!   （`render/_internal/content_list/common.py:118-122`）。拿不到页面框的页
//!   （PDF 文字层的 `ContentExtent`）**不给 bbox**（`None` → 省略该键），
//!   绝不伪造分母——见 [`crate::docir::PageDims`]。
//! - 类型名：24 项**逐字**抄 `types.py::ContentTypeV2`（见 [`ct`]），不自造。
//! - span：`{"type":"text","content":...}` + 可选 `style` 列表，样式名与顺序
//!   逐字抄 `docvortex/schema.py::INLINE_STYLE_ORDER`。
//! - 印章：MinerU 把版面 `seal` 归到 `BlockType.IMAGE` 且 `sub_type="seal"`
//!   （`backend/analysis/pdf/constants.py:92`、`layout.py:43-44`），识别文本写回
//!   该 image 块的 content（`ocr.py:266-297`）。本仓照此投影。
//!
//! 本模块只依赖 [`crate::docir`]（IR 是真相），不碰 pdf/ofd/ocr 内部类型。

use serde_json::{Map, Value, json};

use super::{DocIR, PageDimsKind, PageIR};
use crate::region::{NoiseKind, Region, RegionKind};

/// ContentTypeV2 的 24 个类型名（逐字抄 `mineru/types.py::ContentTypeV2`）。
///
/// 单测 `type_names_match_mineru_verbatim` 逐条钉死字面值——改名必须先改 MinerU 侧。
///
/// 表是**完整性优先**：其中一部分本仓尚无 producer（algorithm /
/// simple_table / complex_table / list / phonetic / md / code_inline 等），保留
/// 常量是为"对齐面"本身，不是为消掉告警——故模块级 allow。
#[allow(dead_code)]
pub mod ct {
    pub const CODE: &str = "code";
    pub const ALGORITHM: &str = "algorithm";
    pub const EQUATION_INTERLINE: &str = "equation_interline";
    pub const IMAGE: &str = "image";
    pub const TABLE: &str = "table";
    pub const CHART: &str = "chart";
    pub const TABLE_SIMPLE: &str = "simple_table";
    pub const TABLE_COMPLEX: &str = "complex_table";
    pub const LIST: &str = "list";
    pub const LIST_TEXT: &str = "text_list";
    pub const LIST_REF: &str = "reference_list";
    pub const INDEX: &str = "index";
    pub const TITLE: &str = "title";
    pub const PARAGRAPH: &str = "paragraph";
    pub const SPAN_TEXT: &str = "text";
    pub const SPAN_EQUATION_INLINE: &str = "equation_inline";
    pub const SPAN_PHONETIC: &str = "phonetic";
    pub const SPAN_MD: &str = "md";
    pub const SPAN_CODE_INLINE: &str = "code_inline";
    pub const PAGE_HEADER: &str = "page_header";
    pub const PAGE_FOOTER: &str = "page_footer";
    pub const PAGE_NUMBER: &str = "page_number";
    pub const PAGE_ASIDE_TEXT: &str = "page_aside_text";
    pub const PAGE_FOOTNOTE: &str = "page_footnote";
}

/// 行内样式名与**顺序**（逐字抄 `docvortex/schema.py::INLINE_STYLE_ORDER`）。
///
/// MinerU 校验 superscript/subscript 互斥（`schema.py:354`）并按此顺序去重输出；
/// 本仓 IR 两个位独立存放（见 `SpanStyles` 注释），**互斥折叠在这里做**——
/// 同时置位时按 MinerU 口径是非法输入，取上标（与 pdf-inspector 的
/// `baseline_shift` 符号约定一致：正 = 上标）。
const INLINE_STYLE_ORDER: [(&str, fn(&crate::region::SpanStyles) -> bool); 7] = [
    ("bold", |s| s.bold),
    ("italic", |s| s.italic),
    ("underline", |s| s.underline),
    ("emphasis", |_| false), // 本仓无证据来源（见 SpanStyles 注释），恒不输出
    ("strikethrough", |s| s.strikethrough),
    ("superscript", |s| s.superscript),
    ("subscript", |s| s.subscript && !s.superscript),
];

/// DocIR → content_list v2（按页分组的 JSON 值）。
///
/// 页序即 IR 页序；**空页也占一个数组槽位**（MinerU 逐页给数组，页下标即页序）。
pub fn to_content_list_v2(doc: &DocIR) -> Vec<Vec<Value>> {
    doc.pages.iter().map(page_items).collect()
}

/// 同上，直接给 JSON 文本（供 CLI / 绑定层落盘或打印）。
pub fn to_content_list_v2_json(doc: &DocIR) -> String {
    let v = to_content_list_v2(doc);
    serde_json::to_string_pretty(&v).unwrap_or_else(|_| "[]".to_string())
}

/// 同 [`to_content_list_v2`]，TODO
fn page_items(page: &PageIR) -> Vec<Value> {
    // #10 INDEX：MinerU 把整个目录块投成**一个** `index` item（v2.py
    // `_render_index`：type=index，content={list_type: text_list, list_items:[…]}），
    // 而本仓每条点线行是一个 Region——故这里把**相邻连续**的 `Index` 行聚合成
    // 一个 item，逐条写进 `list_items`。判据只有"页内阅读序相邻"：若目次被别的
    // body 行插开会拆成两个 index item，这与 MinerU 的 IndexBlock 口径同构
    // （它同样不会因为中间隔着正文就把两段目录拼成一个块）。
    // #10 切片 4：相邻连续的 list_item 段落同理聚合成一个 `list` item（v2.py
    // `_render_list`），被 Index 行/正文行/家具打断即拆开。
    // #10 补全：相邻连续的 `Reference` 条目聚合成一个 `reference_list` item
    // （v2.py `_reference_list_item`），被打断即拆开——与 index/list run 同构。
    // #10 chart 票：相邻连续的 `Chart` 行聚合成**一个** `chart` item
    // （一张图 = 一个 `ChartBlock`；`content` 收全部图内文字行），与
    // render层 `collapse_chart_runs` 的聚合同构，两端口径一致。
    let mut items: Vec<Value> = Vec::new();
    let mut index_run: Vec<&Region> = Vec::new();
    let mut list_run: Vec<&Region> = Vec::new();
    let mut reference_run: Vec<&Region> = Vec::new();
    let mut chart_run: Vec<&Region> = Vec::new();
    for r in page.regions.iter().filter(|r| !r.is_continues_prev()) {
        if r.kind == RegionKind::Index {
            if let Some(v) = list_run_item(std::mem::take(&mut list_run), page) {
                items.push(v);
            }
            if let Some(v) = reference_run_item(std::mem::take(&mut reference_run), page) {
                items.push(v);
            }
            if let Some(v) = chart_run_item(std::mem::take(&mut chart_run), page) {
                items.push(v);
            }
            index_run.push(r);
            continue;
        }
        if r.kind == RegionKind::Chart {
            if let Some(v) = index_run_item(std::mem::take(&mut index_run), page) {
                items.push(v);
            }
            if let Some(v) = list_run_item(std::mem::take(&mut list_run), page) {
                items.push(v);
            }
            if let Some(v) = reference_run_item(std::mem::take(&mut reference_run), page) {
                items.push(v);
            }
            chart_run.push(r);
            continue;
        }
        if r.kind == RegionKind::Reference {
            if let Some(v) = index_run_item(std::mem::take(&mut index_run), page) {
                items.push(v);
            }
            if let Some(v) = list_run_item(std::mem::take(&mut list_run), page) {
                items.push(v);
            }
            if let Some(v) = chart_run_item(std::mem::take(&mut chart_run), page) {
                items.push(v);
            }
            reference_run.push(r);
            continue;
        }
        if r.list_item && r.kind == RegionKind::Body {
            if let Some(v) = index_run_item(std::mem::take(&mut index_run), page) {
                items.push(v);
            }
            if let Some(v) = reference_run_item(std::mem::take(&mut reference_run), page) {
                items.push(v);
            }
            if let Some(v) = chart_run_item(std::mem::take(&mut chart_run), page) {
                items.push(v);
            }
            list_run.push(r);
            continue;
        }
        if let Some(v) = index_run_item(std::mem::take(&mut index_run), page) {
            items.push(v);
        }
        if let Some(v) = list_run_item(std::mem::take(&mut list_run), page) {
            items.push(v);
        }
        if let Some(v) = reference_run_item(std::mem::take(&mut reference_run), page) {
            items.push(v);
        }
        if let Some(v) = chart_run_item(std::mem::take(&mut chart_run), page) {
            items.push(v);
        }
        if let Some(v) = region_item(r, page) {
            items.push(v);
        }
    }
    if let Some(v) = index_run_item(index_run, page) {
        items.push(v);
    }
    if let Some(v) = list_run_item(list_run, page) {
        items.push(v);
    }
    if let Some(v) = reference_run_item(reference_run, page) {
        items.push(v);
    }
    if let Some(v) = chart_run_item(chart_run, page) {
        items.push(v);
    }
    items
}

/// 一组相邻连续的 list_item 段落 → 单个 `list` item；空组 → `None`。
///
/// 形态逐字对齐 v2.py `_render_list`（text_list 分支）：
/// `{"type":"list","content":{"list_type":"text_list","list_items":
/// [{"item_type":"text","item_content":[spans]},…],"attribute":…}}`。
/// `attribute` 投票对齐 `infer_list_attribute`：成员 marker 全 `ordered`
/// （字母点式 `a.`，`marker_is_ordered`）→ `"ordered"`，否则 `"unordered"`
/// （bullet/括号式/中文形态在 MinerU kind 里是 unordered/explicit/none，
/// 全归 unordered）。bbox = 成员框并集（同 [`index_run_item`] 口径）。
fn list_run_item(run: Vec<&Region>, page: &PageIR) -> Option<Value> {
    if run.is_empty() {
        return None;
    }
    let list_items: Vec<Value> = run
        .iter()
        .map(|r| {
            json!({
                "item_type": ct::SPAN_TEXT,
                "item_content": spans_of(r),
            })
        })
        .collect();
    if list_items.is_empty() {
        return None; // 全空行不产出 item（与 index run 同口径）
    }
    let ordered = run
        .iter()
        .all(|r| crate::reading_order::marker_is_ordered(&r.text) == Some(true));
    let mut item = Map::new();
    item.insert("type".into(), Value::String(ct::LIST.into()));
    item.insert(
        "content".into(),
        json!({
            "list_type": ct::LIST_TEXT,
            "list_items": list_items,
            "attribute": if ordered { "ordered" } else { "unordered" },
        }),
    );
    if let Some(b) = bbox_union(&run, page) {
        item.insert("bbox".into(), json!(b));
    }
    Some(Value::Object(item))
}

/// 一组相邻连续的 `Index` 行 → 单个 `index` item；空组 → `None`。
fn index_run_item(run: Vec<&Region>, page: &PageIR) -> Option<Value> {    if run.is_empty() {
        return None;
    }
    let list_items: Vec<Value> = run
        .iter()
        .map(|r| {
            json!({
                "item_type": ct::SPAN_TEXT,
                "item_content": spans_of(r),
            })
        })
        .collect();
    if list_items.is_empty() {
        return None; // 全空行不产出 item（与单体 branch 同口径）
    }
    let mut item = Map::new();
    item.insert("type".into(), Value::String(ct::INDEX.into()));
    item.insert(
        "content".into(),
        json!({ "list_type": ct::LIST_TEXT, "list_items": list_items }),
    );
    if let Some(b) = bbox_union(&run, page) {
        item.insert("bbox".into(), json!(b));
    }
    Some(Value::Object(item))
}

/// 一组相邻连续的 `Reference` 条目 → 单个 `reference_list` item；空组 → `None`。
///
/// 形态逐字对齐 v2.py `_reference_list_item`（:167-184）：
/// `{"type":"list","content":{"list_type":"reference_list","list_items":
/// [{"item_type":"text","item_content":[spans]},…]}}`——**无 `attribute` 键**
/// （那是 text_list 独有，`_render_list` 里 `if list_type == "text_list"` 才给）。
/// bbox = 成员框并集（同 [`index_run_item`] 口径）。
fn reference_run_item(run: Vec<&Region>, page: &PageIR) -> Option<Value> {
    if run.is_empty() {
        return None;
    }
    let list_items: Vec<Value> = run
        .iter()
        .map(|r| {
            json!({
                "item_type": ct::SPAN_TEXT,
                "item_content": spans_of(r),
            })
        })
        .collect();
    if list_items.is_empty() {
        return None; // 全空行不产出 item（与 index/list run 同口径）
    }
    let mut item = Map::new();
    item.insert("type".into(), Value::String(ct::LIST.into()));
    item.insert(
        "content".into(),
        json!({ "list_type": ct::LIST_REF, "list_items": list_items }),
    );
    if let Some(b) = bbox_union(&run, page) {
        item.insert("bbox".into(), json!(b));
    }
    Some(Value::Object(item))
}

/// 一组相邻连续的 `Chart` 行 → 单个 `chart` item；空组 → `None`。
///
/// 形态逐字对齐 v2.py `_render_chart`（:263-281）的 `content` 四键。
/// `content` 是**全部成员行的 span 顺次拼接**（图内文字在 markdown 端已按
/// 定案丢弃，这里是它唯一的幸存处，见 `region_item` 的 `Chart` 分支注释）。
/// bbox = 成员框并集（同 [`index_run_item`] 口径）——即整张图的外接框。
fn chart_run_item(run: Vec<&Region>, page: &PageIR) -> Option<Value> {
    if run.is_empty() {
        return None;
    }
    let content: Vec<Value> = run.iter().flat_map(|r| spans_of(r)).collect();
    if content.is_empty() {
        return None; // 全空行不产出 item（与 index/list run 同口径）
    }
    let mut item = Map::new();
    item.insert("type".into(), Value::String(ct::CHART.into()));
    item.insert(
        "content".into(),
        json!({
            "image_source": {"path": ""},
            "content": content,
            "chart_caption": [],
            "chart_footnote": [],
        }),
    );
    if let Some(b) = bbox_union(&run, page) {
        item.insert("bbox".into(), json!(b));
    }
    Some(Value::Object(item))
}

/// 一组 region 的 bbox 并集（聚合块的几何），取不到归一分母或全无几何 → `None`。
fn bbox_union(rs: &[&Region], page: &PageIR) -> Option<[i32; 4]> {
    let mut acc: Option<[i32; 4]> = None;
    for r in rs {
        let Some(b) = bbox_of(r, page) else { continue };
        acc = Some(match acc {
            None => b,
            Some(a) => [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])],
        });
    }
    acc
}

/// 单个 Region → V2 item；无对应 V2 类型（或内容为空）→ `None`（不产出）。
///
/// 跳过 `continues_prev` 块：其内容已并入首表页的那份（见该字段的注释），
/// 投影层若当正文输出就会**重行**——与渲染层同一口径。
fn region_item(r: &Region, page: &PageIR) -> Option<Value> {
    let (kind, content) = match &r.kind {
        // 标题：`heading_level` 是 IR 数据（#6 第 2 步），不必反解 `#` 字面量。
        // 级别缺失（标题语义但无级别）按 MinerU 的 `title` 兜 1 级。
        _ if r.heading_level.is_some() => (
            ct::TITLE,
            json!({
                "title_content": spans_of(r),
                "level": r.heading_level.unwrap_or(1),
            }),
        ),
        RegionKind::Body | RegionKind::PreRendered => {
            (ct::PARAGRAPH, json!({ "paragraph_content": spans_of(r) }))
        }
        // #10 补全：aside 独立 item（v2.py `_page_content_type`：ASIDE_TEXT →
        // "page_aside_text"，content 键 = `f"{content_type}_content"`，与
        // header/footer/footnote 同构）。markdown 形态仍是普通段落（渲染层
        // 与 Body 同道），只有结构化出口分列。
        RegionKind::Aside => (
            ct::PAGE_ASIDE_TEXT,
            json!({ "page_aside_text_content": spans_of(r) }),
        ),
        RegionKind::Grid(_) | RegionKind::TableHtml => (
            ct::TABLE,
            json!({
                "image_source": {"path": ""},
                "table_caption": [],
                "table_footnote": [],
                "html": html_of(r),
                // 复杂度：MinerU 用 `classify_table(html)` 判 simple/complex，
                // 本仓无该判别器（且它依赖嵌套表结构），故恒给 null——宁缺勿造。
                "table_type": Value::Null,
                "table_nest_level": 0,
            }),
        ),
        RegionKind::Code => (
            ct::CODE,
            json!({
                "code_caption": [],
                "code_content": spans_of(r),
                "code_footnote": [],
                "code_language": "txt",
            }),
        ),
        RegionKind::Formula => (
            ct::EQUATION_INTERLINE,
            json!({
                "math_content": r.text.trim(),
                "math_type": "latex",
                "image_source": {"path": ""},
            }),
        ),
        RegionKind::Image => (
            ct::IMAGE,
            json!({
                "image_source": {"path": ""},
                "image_caption": [],
                "image_footnote": [],
            }),
        ),
        // #10 chart 票。形态逐字抄 MinerU `v2.py::_render_chart`（:263-281）：
        // `image_source` + `content`（图内文字，`render_embedded_content(body.content)`）
        // + `chart_caption` + `chart_footnote`。
        //
        // **与 basic 档的差别是有意的**：`content` 放本仓 OCR 读到的图内文字
        // （basic 恒空串——`ChartBodyBlock(ImagePayloadContentBlock)` 的
        // `content: str` 是类型层面的强制丢弃，为 VLM 二次填充预留，
        // `postprocess/page_blocks.py:90-91` 置空）。markdown 端已按定案丢弃
        // 这部分（只留 `<!-- chart -->`），结构化出口是它唯一的幸存处，
        // 丢了就等于本仓白拿了一次 OCR。
        //
        // `image_source.path` 恒空串：本仓不产图片资产（markdown 端因此不写
        // `![](…)` 死链），与 `Image` 分支同口径。
        //
        // `chart_caption`/`chart_footnote` 恒空数组：图注在本仓是**独立的
        // `FigureTitle` 版面元素**、走普通正文流（见 `RegionKind::Chart` 文档
        // 的取证），不是本块内的附属字段——这正是 basic 自身缺陷（`chart_caption`
        // 与 `chart_footnote` 把同一图注重复输出两次）要避免的形状。
        RegionKind::Chart => (
            ct::CHART,
            json!({
                "image_source": {"path": ""},
                "content": spans_of(r),
                "chart_caption": [],
                "chart_footnote": [],
            }),
        ),
        RegionKind::Index => (
            ct::INDEX,
            json!({ "list_type": ct::LIST_TEXT, "list_items": [] }),
        ),
        RegionKind::Footnote => (
            ct::PAGE_FOOTNOTE,
            json!({ "page_footnote_content": spans_of(r) }),
        ),
        // Reference 条目不出单条 item——相邻连续条目在 [`page_items`] 聚合成
        // `reference_list`（v2.py `_reference_list_item`）。此分支仅供 match
        // 穷尽性，正常不可达；兜底按 paragraph（markdown 形态本就是普通段落）。
        RegionKind::Reference => {
            (ct::PARAGRAPH, json!({ "paragraph_content": spans_of(r) }))
        }
        RegionKind::Noise(NoiseKind::Header) => (
            ct::PAGE_HEADER,
            json!({ "page_header_content": spans_of(r) }),
        ),
        RegionKind::Noise(NoiseKind::Footer) => (
            ct::PAGE_FOOTER,
            json!({ "page_footer_content": spans_of(r) }),
        ),
        RegionKind::Noise(NoiseKind::PageNumber) => (
            ct::PAGE_NUMBER,
            json!({ "page_number_content": spans_of(r) }),
        ),
        // 印章：MinerU 归 image + sub_type="seal"，识别文本写回块内容
        // （`constants.py:92` / `layout.py:43-44` / `ocr.py:266-297`）。
        RegionKind::Noise(NoiseKind::Seal) => (
            ct::IMAGE,
            json!({
                "image_source": {"path": ""},
                "image_caption": [],
                "image_footnote": [],
                "content": spans_of(r),
            }),
        ),
    };
    if content_is_empty(&content) {
        return None; // 空内容不产出 item（对齐"空块不入 content_list"）
    }
    let mut item = Map::new();
    item.insert("type".into(), Value::String(kind.into()));
    if kind == ct::IMAGE && matches!(r.kind, RegionKind::Noise(NoiseKind::Seal)) {
        item.insert("sub_type".into(), Value::String("seal".into()));
    }
    item.insert("content".into(), content);
    if let Some(b) = bbox_of(r, page) {
        item.insert("bbox".into(), json!(b));
    }
    Some(Value::Object(item))
}

/// Region → V2 span 数组（#6 第 4 步的 `spans` 是消费点）。
///
/// `spans` 为空（OFD/OCR 等无样式证据源）→ 用 `text` 产单条 plain span，
/// 绝不产空数组：空 content 的 item 会被 [`content_is_empty`] 丢掉，正文就丢了。
fn spans_of(r: &Region) -> Vec<Value> {
    if !r.spans.is_empty() {
        return r.spans.iter().map(span_value).collect();
    }
    let t = r.text.trim();
    if t.is_empty() {
        return Vec::new();
    }
    vec![json!({ "type": ct::SPAN_TEXT, "content": t })]
}

fn span_value(s: &crate::region::Span) -> Value {
    let text = s.text.trim();
    if text.is_empty() {
        return json!({ "type": ct::SPAN_TEXT, "content": "" });
    }
    let mut v = Map::new();
    v.insert("type".into(), Value::String(ct::SPAN_TEXT.into()));
    v.insert("content".into(), Value::String(text.into()));
    // 与 MinerU `_serialize_v2_span` 同口径：无样式就不给 `style` 键。
    if !s.styles.is_plain() {
        let styles: Vec<&str> = INLINE_STYLE_ORDER
            .iter()
            .filter(|(_, f)| f(&s.styles))
            .map(|(n, _)| *n)
            .collect();
        if !styles.is_empty() {
            v.insert("style".into(), json!(styles));
        }
    }
    Value::Object(v)
}

/// 表格 html：`TableHtml` 直接带，`Grid` 由渲染层的网格表渲染器产出。
fn html_of(r: &Region) -> String {
    match &r.kind {
        RegionKind::TableHtml => r.text.clone(),
        RegionKind::Grid(g) => crate::table_grid::table_grid_to_html(g),
        _ => String::new(),
    }
}

/// content 是否为空（判定"这个块有没有可交付内容"）。
///
/// 表格看 html（表可以没有正文但有结构）；其余看是否含非空文本串。
fn content_is_empty(v: &Value) -> bool {
    fn has_text(v: &Value) -> bool {
        match v {
            Value::String(s) => !s.trim().is_empty(),
            Value::Array(a) => a.iter().any(has_text),
            Value::Object(o) => o.values().any(has_text),
            _ => false,
        }
    }
    match v.get("html") {
        Some(h) => h.as_str().map(|s| s.trim().is_empty()).unwrap_or(true),
        None => !has_text(v),
    }
}

/// bbox → 0–1000 归一化整数；两种情形**省略键**（`None`）而不是造数：
/// 1. 页尺寸不可归一化（PDF 文字层的 `ContentExtent`、整页转正页）——没有合法分母；
/// 2. 区块本身是退化框（无几何的纯文本块，见 [`Region::has_geometry`]）——
///    输出 `[0,0,0,0]` 等于伪造"在页左上角、零尺寸"的坐标。
///
/// `PageBoxPdfPt`（#11b-v2，PDF 文字层页框）的 y 额外做 **baseline-flip →
/// top-down** 换算：本仓 IR 的行框是 `y_min = -baseline`、`y_max = -baseline+em`
/// （`pdf/text_layer.rs::push_line_region`，非纯 `-y` 翻转），归一化前先换回
/// "页顶起算"的视觉坐标：`y0_top = H + 2·y_min - y_max`、`y1_bottom = H + y_min`
/// （公式推导见 `PageDimsKind::PageBoxPdfPt` doc）。x 不受 y 语义影响，照常除 W。
fn bbox_of(r: &Region, page: &PageIR) -> Option<[i32; 4]> {
    if !page.dims.normalizable() || !r.has_geometry() {
        return None;
    }
    let (w, h) = (page.dims.w, page.dims.h);
    let n = |v: f32, d: f32| -> i32 { ((v / d) * 1000.0).round().clamp(0.0, 1000.0) as i32 };
    let (y0, y1) = if page.dims.kind == PageDimsKind::PageBoxPdfPt {
        (h + 2.0 * r.y_min - r.y_max, h + r.y_min)
    } else {
        (r.y_min, r.y_max)
    };
    Some([n(r.x_min, w), n(y0, h), n(r.x_max, w), n(y1, h)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docir::{DocIR, PageDims, PageSource};
    use crate::region::{NoiseKind, Region, RegionKind, Span, SpanStyles};

    /// **验收判据之一**：24 个类型名与 MinerU `ContentTypeV2` 逐字等值
    /// （`mineru/types.py:197-223`）。改任一个字面值都会红。
    #[test]
    fn type_names_match_mineru_verbatim() {
        assert_eq!(
            [
                ct::CODE, ct::ALGORITHM, ct::EQUATION_INTERLINE, ct::IMAGE, ct::TABLE,
                ct::CHART, ct::TABLE_SIMPLE, ct::TABLE_COMPLEX, ct::LIST, ct::LIST_TEXT,
                ct::LIST_REF, ct::INDEX, ct::TITLE, ct::PARAGRAPH, ct::SPAN_TEXT,
                ct::SPAN_EQUATION_INLINE, ct::SPAN_PHONETIC, ct::SPAN_MD,
                ct::SPAN_CODE_INLINE, ct::PAGE_HEADER, ct::PAGE_FOOTER, ct::PAGE_NUMBER,
                ct::PAGE_ASIDE_TEXT, ct::PAGE_FOOTNOTE,
            ],
            [
                "code", "algorithm", "equation_interline", "image", "table", "chart",
                "simple_table", "complex_table", "list", "text_list", "reference_list",
                "index", "title", "paragraph", "text", "equation_inline", "phonetic", "md",
                "code_inline", "page_header", "page_footer", "page_number",
                "page_aside_text", "page_footnote",
            ]
        );
    }

    fn page_with(dims: PageDims, regions: Vec<Region>) -> DocIR {
        let mut d = DocIR::default();
        d.push_page(0, PageSource::Ocr, regions, dims);
        d
    }

    fn body(text: &str) -> Region {
        Region::new(10., 100., 20., 120., text.to_string())
    }

    /// 正文 → paragraph；span 空时回落到 `text`（不产空 content）。
    #[test]
    fn body_region_projects_to_paragraph() {
        let d = page_with(PageDims::page_box_px(1000, 1000), vec![body("你好世界")]);
        let v = to_content_list_v2(&d);
        assert_eq!(v[0].len(), 1);
        assert_eq!(v[0][0]["type"], "paragraph");
        assert_eq!(v[0][0]["content"]["paragraph_content"][0]["content"], "你好世界");
        assert_eq!(v[0][0]["content"]["paragraph_content"][0]["type"], "text");
        // 无样式 → 不给 style 键（同 `_serialize_v2_span`）
        assert!(v[0][0]["content"]["paragraph_content"][0].get("style").is_none());
    }

    /// `heading_level` 是 IR 数据 → 直接投影 `title` + `level`，无需反解 `#`。
    #[test]
    fn heading_level_projects_to_title() {
        let mut r = body("第一章");
        r.heading_level = Some(2);
        let d = page_with(PageDims::page_box_px(1000, 1000), vec![r]);
        let v = to_content_list_v2(&d);
        assert_eq!(v[0][0]["type"], "title");
        assert_eq!(v[0][0]["content"]["level"], 2);
        assert_eq!(v[0][0]["content"]["title_content"][0]["content"], "第一章");
    }

    /// span 样式按 `INLINE_STYLE_ORDER` 输出；上下标同时置位时按 MinerU 口径取上标。
    #[test]
    fn span_styles_follow_mineru_order_and_are_exclusive() {
        let mut r = body("");
        r.spans = vec![Span::new(
            "上标",
            SpanStyles { bold: true, superscript: true, subscript: true, ..Default::default() },
        )];
        let d = page_with(PageDims::page_box_px(1000, 1000), vec![r]);
        let v = to_content_list_v2(&d);
        let style = &v[0][0]["content"]["paragraph_content"][0]["style"];
        // 顺序 = bold, italic, underline, emphasis, strikethrough, superscript, subscript
        assert_eq!(style, &json!(["bold", "superscript"]));
    }

    /// bbox = 0–1000 归一化整数；页尺寸不可归一化 → **省略** bbox 键。
    #[test]
    fn bbox_is_normalized_to_0_1000_or_omitted() {
        // Region::new(x_min, x_max, y_min, y_max, text)；页 1000×2000
        let px = page_with(
            PageDims::page_box_px(1000, 2000),
            vec![Region::new(100., 500., 200., 400., "x".to_string())],
        );
        let v = to_content_list_v2(&px);
        assert_eq!(v[0][0]["bbox"], json!([100, 100, 500, 200]));
        // PDF 文字层的 ContentExtent：分母不是页面框 → 不给 bbox，不伪造
        let pt = page_with(
            PageDims::extent_pt(612., 792.),
            vec![Region::new(10., 50., 20., 40., "x".to_string())],
        );
        let v = to_content_list_v2(&pt);
        assert!(v[0][0].get("bbox").is_none(), "不可归一化时不得给 bbox");
    }

    /// 退化框（producer 只给文本没给几何，OFD 文字层正文 / 成品表格块即此类）
    /// 必须省略 `bbox`，**不得**输出 `[0,0,0,0]`——那是伪造坐标，不是"空"。
    #[test]
    fn degenerate_box_omits_bbox_instead_of_zero_quad() {
        let d = page_with(
            PageDims::page_box_px(1000, 1000),
            vec![Region::new(0., 0., 0., 0., "无框正文".to_string())],
        );
        let v = to_content_list_v2(&d);
        assert_eq!(v[0][0]["type"], json!("paragraph"), "文本照常产出");
        assert!(
            v[0][0].get("bbox").is_none(),
            "退化框不得给 bbox（给了就是 [0,0,0,0] 伪坐标）"
        );
    }

    /// #11b-v2：`PageBoxPdfPt` 页的 bbox 走 **baseline-flip → top-down** 换算
    /// （`y0_top = H + 2·y_min - y_max`、`y1_bottom = H + y_min`）。
    /// roundtrip 验收：换算结果反算回 pt 必须等于该行**视觉框**（距页顶）——
    /// 行框 `[baseline, baseline+em]`（box 帧 y 向上）的视觉上下沿距页顶是
    /// `H - baseline - em` 与 `H - baseline`。页顶行顶格 0、页底行触底 1000。
    #[test]
    fn bbox_pdf_pt_baseline_flip_roundtrip() {
        let (w, h) = (595.0_f32, 842.0_f32); // A4
        let mk = |y_min: f32, y_max: f32| {
            page_with(
                PageDims::page_box_pdf_pt(w, h),
                vec![Region::new(50., 300., y_min, y_max, "行".to_string())],
            )
        };
        let v = |q: serde_json::Value| {
            q.as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_i64().unwrap())
                .collect::<Vec<_>>()
        };
        // 中部行：baseline=300、em=10 → 视觉框距顶 [532, 542]。
        let got = v(to_content_list_v2(&mk(-300., -290.))[0][0]["bbox"].clone());
        assert_eq!(got, vec![84, 632, 504, 644], "roundtrip: 632/1000·842≈532=H-300-10 ✓");
        // 页顶行：baseline=832（=H-em）→ 顶格 y0=0。
        let got = v(to_content_list_v2(&mk(-832., -822.))[0][0]["bbox"].clone());
        assert_eq!(got[1], 0, "页顶行视觉上沿 = 0");
        // 页底行：baseline=0 → 触底 y1=1000。
        let got = v(to_content_list_v2(&mk(0., 10.))[0][0]["bbox"].clone());
        assert_eq!(got[3], 1000, "页底行视觉下沿 = 1000");
    }

    /// 页面家具：Noise(Header/Footer/PageNumber) 与 Footnote 各自投影到对应类型，
    /// 与"正文"分列（这正是 #6 第 5 步把它们标出来的目的）。
    #[test]
    fn furniture_projects_to_page_types() {
        let mk = |k: RegionKind| {
            let mut r = body("页眉");
            r.kind = k;
            r
        };
        let d = page_with(
            PageDims::page_box_px(1000, 1000),
            vec![
                mk(RegionKind::Noise(NoiseKind::Header)),
                mk(RegionKind::Noise(NoiseKind::Footer)),
                mk(RegionKind::Noise(NoiseKind::PageNumber)),
                mk(RegionKind::Footnote),
            ],
        );
        let v = to_content_list_v2(&d);
        let types: Vec<&str> = v[0].iter().map(|i| i["type"].as_str().unwrap()).collect();
        assert_eq!(types, vec!["page_header", "page_footer", "page_number", "page_footnote"]);
        assert_eq!(v[0][0]["content"]["page_header_content"][0]["content"], "页眉");
    }

    /// 印章 → image + `sub_type="seal"`（MinerU 同口径：
    /// `constants.py:92` / `layout.py:43-44` / `ocr.py:266-297`）。
    #[test]
    fn seal_projects_to_image_with_seal_subtype() {
        let mut r = body("北京测试科技有限公司");
        r.kind = RegionKind::Noise(NoiseKind::Seal);
        let d = page_with(PageDims::page_box_px(1000, 1000), vec![r]);
        let v = to_content_list_v2(&d);
        assert_eq!(v[0][0]["type"], "image");
        assert_eq!(v[0][0]["sub_type"], "seal");
        assert_eq!(
            v[0][0]["content"]["content"][0]["content"],
            "北京测试科技有限公司"
        );
    }

    /// `continues_prev` 块**不得**投影（内容已并入首表页，投了就重行）。
    #[test]
    fn continues_prev_regions_are_skipped() {
        let mut kept = body("首表页");
        let mut stub = body("续页");
        stub.continues_prev = Some(true);
        let d = page_with(PageDims::page_box_px(1000, 1000), vec![kept.clone(), stub]);
        let _ = &mut kept;
        let v = to_content_list_v2(&d);
        assert_eq!(v[0].len(), 1, "续接块必须跳过");
        assert_eq!(v[0][0]["content"]["paragraph_content"][0]["content"], "首表页");
    }

    /// 空块不产出 item（对齐"空块不入 content_list"）。
    #[test]
    fn empty_regions_produce_no_items() {
        let d = page_with(
            PageDims::page_box_px(1000, 1000),
            vec![body("   "), body("")],
        );
        assert!(to_content_list_v2(&d)[0].is_empty());
    }

    /// 表格：html 非空即可投影（表可以没有正文但有结构）。
    #[test]
    fn table_projects_with_html() {
        let mut r = Region::new(0., 0., 10., 10., String::new());
        r.kind = RegionKind::TableHtml;
        r.text = "<table><tr><td>a</td></tr></table>".to_string();
        let d = page_with(PageDims::page_box_px(1000, 1000), vec![r]);
        let v = to_content_list_v2(&d);
        assert_eq!(v[0][0]["type"], "table");
        assert!(v[0][0]["content"]["html"].as_str().unwrap().contains("<td>a</td>"));
        // 复杂度判别器本仓没有 → 恒 null（宁缺勿造）
        assert!(v[0][0]["content"]["table_type"].is_null());
    }

    /// 顶层形状 = 按页分组；空页也占一个槽位（MinerU 逐页给数组）。
    /// #10 INDEX：相邻连续的目次条目聚合成**一个** `index` item（v2.py
    /// `_render_index` 的 `list_type: text_list` + 逐条 `list_items`）；
    /// 中间隔着正文行则拆成两个 index item（与 MinerU 的 IndexBlock 同构）。
    #[test]
    fn index_runs_collapse_into_one_index_item() {
        let idx = |t: &str| {
            let mut r = body(t);
            r.kind = RegionKind::Index;
            r
        };
        let d = page_with(
            PageDims::page_box_px(1000, 1000),
            vec![
                idx("前言…………IV"),
                idx("引言…………V"),
                body("正文一段"),
                idx("1 范围…………1"),
            ],
        );
        let v = to_content_list_v2(&d);
        let types: Vec<&str> = v[0].iter().map(|i| i["type"].as_str().unwrap()).collect();
        assert_eq!(types, vec!["index", "paragraph", "index"]);
        assert_eq!(v[0][0]["content"]["list_type"], "text_list");
        assert_eq!(
            v[0][0]["content"]["list_items"][1]["item_content"][0]["content"],
            "引言…………V"
        );
        // item_type 逐字抄 MinerU v2 的 `{"item_type": "text", ...}`
        assert_eq!(v[0][0]["content"]["list_items"][0]["item_type"], "text");
        assert_eq!(v[0][2]["content"]["list_items"][0]["item_content"][0]["content"], "1 范围…………1");
    }

    /// #10 切片 4：相邻连续的 list_item 段落 → **一个** `list` item（v2.py
    /// `_render_list`：text_list + 逐条 list_items + attribute）；被正文段
    /// 插开则拆成两个；非 marker 段照常 paragraph。
    #[test]
    fn list_marker_runs_collapse_into_one_list_item() {
        let li = |t: &str| {
            let mut r = body(t);
            r.list_item = true;
            r
        };
        let d = page_with(
            PageDims::page_box_px(1000, 1000),
            vec![
                li("a) 通用要求"),
                li("A、总则"),
                body("正文一段"),
                li("- 引导启动项"),
            ],
        );
        let v = to_content_list_v2(&d);
        let types: Vec<&str> = v[0].iter().map(|i| i["type"].as_str().unwrap()).collect();
        assert_eq!(types, vec!["list", "paragraph", "list"], "marker 行聚合、正文隔开");
        // attribute：a) explicit + A、none → 非 ordered 全员 → unordered
        assert_eq!(v[0][0]["content"]["list_type"], "text_list");
        assert_eq!(v[0][0]["content"]["attribute"], "unordered");
        assert_eq!(v[0][0]["content"]["list_items"][0]["item_type"], "text");
        assert_eq!(
            v[0][0]["content"]["list_items"][0]["item_content"][0]["content"],
            "a) 通用要求"
        );
        assert_eq!(v[0][2]["content"]["list_items"][0]["item_content"][0]["content"], "- 引导启动项");
        // bbox = 成员并集（同 index run 口径）
        let b: Vec<i64> = v[0][0]["bbox"].as_array().unwrap().iter().map(|x| x.as_i64().unwrap()).collect();
        assert_eq!(b, vec![10, 20, 100, 120], "两个成员框并集（body 基准框 10..100/20..120）");
    }

    /// attribute 投票：全字母点式（MinerU 唯一 ordered kind）→ `"ordered"`。
    #[test]
    fn all_letter_dot_markers_vote_ordered() {
        let li = |t: &str| {
            let mut r = body(t);
            r.list_item = true;
            r
        };
        let d = page_with(PageDims::page_box_px(1000, 1000), vec![li("a. 第一项"), li("b. 第二项")]);
        let v = to_content_list_v2(&d);
        assert_eq!(v[0].len(), 1);
        assert_eq!(v[0][0]["type"], "list");
        assert_eq!(v[0][0]["content"]["attribute"], "ordered");
        // bullet 混入 → unordered（infer_list_attribute 的 all 判据）
        let d2 = page_with(PageDims::page_box_px(1000, 1000), vec![li("a. 第一项"), li("• 要点")]);
        let v2 = to_content_list_v2(&d2);
        assert_eq!(v2[0][0]["content"]["attribute"], "unordered");
    }

    /// #10 补全：aside 独立 item——`page_aside_text` + `page_aside_text_content`
    /// 键（v2.py `_page_content_type` + `f"{content_type}_content"`，与
    /// header/footer/footnote 同构）。不再混入 paragraph。
    #[test]
    fn aside_projects_to_page_aside_text() {
        let mut r = body("旁注一行");
        r.kind = RegionKind::Aside;
        let d = page_with(PageDims::page_box_px(1000, 1000), vec![r]);
        let v = to_content_list_v2(&d);
        assert_eq!(v[0][0]["type"], "page_aside_text");
        assert_eq!(
            v[0][0]["content"]["page_aside_text_content"][0]["content"],
            "旁注一行"
        );
    }

    /// #10 补全：相邻 Reference 条目聚合成**一个** `reference_list` item
    /// （v2.py `_reference_list_item`：list_type=reference_list + 逐条
    /// list_items，**无 attribute**——text_list 独有）；被正文行打断拆开。
    #[test]
    fn reference_runs_collapse_into_reference_list() {
        let rf = |t: &str| {
            let mut r = body(t);
            r.kind = RegionKind::Reference;
            r
        };
        let d = page_with(
            PageDims::page_box_px(1000, 1000),
            vec![
                rf("〔1〕GB/T 19000…"),
                rf("〔2〕GJB 9001B…"),
                body("正文一段"),
                rf("〔3〕GJB 1400…"),
            ],
        );
        let v = to_content_list_v2(&d);
        let types: Vec<&str> = v[0].iter().map(|i| i["type"].as_str().unwrap()).collect();
        assert_eq!(types, vec!["list", "paragraph", "list"], "条目聚合、正文隔开");
        assert_eq!(v[0][0]["content"]["list_type"], "reference_list");
        assert_eq!(
            v[0][0]["content"]["list_items"][1]["item_content"][0]["content"],
            "〔2〕GJB 9001B…"
        );
        assert!(v[0][0]["content"].get("attribute").is_none(), "reference_list 无 attribute");
    }

    /// #10 chart 票：相邻连续 chart 行聚合成**一个** `chart` item
    /// （`_render_chart` 的四键content：`image_source`/`content`/
    /// `chart_caption`/`chart_footnote`），bbox = 成员框并集。
    /// `content` 收图内文字——markdown 端已丢弃，结构化出口是它唯一幸存处。
    #[test]
    fn chart_runs_collapse_into_one_chart_item() {
        let ch = |t: &str, y0: f32, y1: f32| {
            let mut r = body(t);
            r.y_min = y0;
            r.y_max = y1;
            r.kind = RegionKind::Chart;
            r
        };
        let d = page_with(
            PageDims::page_box_px(1000, 1000),
            vec![
                ch("171.2", 367.0, 383.0),
                ch("160", 380.0, 398.0),
                ch("140", 405.0, 425.0),
            ],
        );
        let v = to_content_list_v2(&d);
        assert_eq!(v[0].len(), 1, "一张图 = 一个 item");
        assert_eq!(v[0][0]["type"], "chart");
        let c = &v[0][0]["content"];
        assert_eq!(c["image_source"]["path"], "", "本仓不产图片资产");
        // 三行图内文字都在 content 里
        let texts: Vec<&str> = c["content"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["content"].as_str().unwrap())
            .collect();
        assert_eq!(texts, vec!["171.2", "160", "140"]);
        // 图注字段恒空（本仓图注是独立 FigureTitle 元素，走正文流）
        assert_eq!(c["chart_caption"], json!([]));
        assert_eq!(c["chart_footnote"], json!([]));
        // bbox = 并集 [x_min,y_min,x_max,y_max] → 0–1000 归一化
        assert_eq!(v[0][0]["bbox"], json!([10, 367, 100, 425]));
    }

    #[test]
    fn output_is_grouped_per_page() {        let mut d = DocIR::default();
        d.push_page(0, PageSource::Ocr, vec![body("第一页")], PageDims::page_box_px(1000, 1000));
        d.push_page(1, PageSource::Ocr, vec![], PageDims::page_box_px(1000, 1000));
        d.push_page(2, PageSource::Ocr, vec![body("第三页")], PageDims::page_box_px(1000, 1000));
        let v = to_content_list_v2(&d);
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].len(), 1);
        assert!(v[1].is_empty(), "空页占槽位但为空数组");
        assert_eq!(v[2].len(), 1);
    }
}
