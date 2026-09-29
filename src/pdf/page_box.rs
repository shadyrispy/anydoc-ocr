//! #11b-v2：PDF 文字层页的**可见页框**读取（MediaBox/CropBox 继承解析）。
//!
//! pdf-inspector 的 position API（本仓主链路 `extract_text_items` 所用）返回
//! visible page box 系坐标（`CropBox ∩ MediaBox`，原点=框左下，y 向上），但
//! [`visible_page_box`] 本身是 `pub(crate)`——不 vendor 就拿不到框。此处用已在
//! 依赖树的 lopdf 复刻其读取规则（同源 crate `pdf-inspector 1.24.0
//! extractor/page_box.rs`），逐条对齐：
//!
//! - 组合规则：Media+Crop 都有 → `Crop ∩ Media`，不相交回落 Media；仅 Media →
//!   Media；仅 Crop → `LETTER ∩ Crop`，不相交回落 LETTER；都没有 → `None`。
//! - 继承：MediaBox/CropBox 是可继承属性，沿 `/Parent` 链最多 32 层，**首个
//!   良构 array 胜出**（key 不存在 / 非 array / 元素不足 4 数 / 退化框 → 继续向上）。
//! - 良构：4 数全 finite 且归一后面积 > 0（角点可反序，from_corners 归一化）。
//! - 元素可为 Reference（解引用后取数），多余数值忽略、只读前 4 个。
//!
//! `/Rotate` 页属性**不参与**：pdf-inspector 的 position API 同样不应用它
//! （`PageBox` doc 注释 "/Rotate is not applied"），两侧口径天然一致。整页
//! 内容流转正（`correct_rotated_page`，Ccw/Cw）的页不在此处理——其坐标帧的
//! y 语义与 baseline-flip 换算前提不兼容且无实测样本，调用方按"宁缺勿造"
//! 维持 `ContentExtent`（见 `build_text_docir` 的 dims 决策注释）。

use std::collections::BTreeMap;

use lopdf::{Document, Object};

/// 可见页框（raw PDF user space，已归一为 `x0 < x1`、`y0 < y1`）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PageBox {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

/// 无任何可用框时渲染器的假设：US Letter（与 pdf-inspector 同口径）。
const LETTER: PageBox = PageBox { x0: 0.0, y0: 0.0, x1: 612.0, y1: 792.0 };

impl PageBox {
    pub fn width(&self) -> f32 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> f32 {
        self.y1 - self.y0
    }

    /// 两角点 → 归一化框；非 finite 或零面积 → `None`。
    fn from_corners(ax: f32, ay: f32, bx: f32, by: f32) -> Option<PageBox> {
        if ![ax, ay, bx, by].iter().all(|v| v.is_finite()) {
            return None;
        }
        let b = PageBox {
            x0: ax.min(bx),
            y0: ay.min(by),
            x1: ax.max(bx),
            y1: ay.max(by),
        };
        (b.x1 > b.x0 && b.y1 > b.y0).then_some(b)
    }

    /// 交集；不相交 → `None`（直接比较，不经 from_corners——角点归一化会把
    /// 不相交对变成幻影框，pdf-inspector 同注释）。
    fn intersect(&self, other: &PageBox) -> Option<PageBox> {
        let b = PageBox {
            x0: self.x0.max(other.x0),
            y0: self.y0.max(other.y0),
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
        };
        (b.x1 > b.x0 && b.y1 > b.y0).then_some(b)
    }
}

/// 渲染器同款可见页框判定（`pdf-inspector extractor/page_box.rs:127-136`）。
fn visible_page_box(doc: &Document, page_id: lopdf::ObjectId) -> Option<PageBox> {
    let media = find_inherited_box(doc, page_id, b"MediaBox");
    let crop = find_inherited_box(doc, page_id, b"CropBox");
    match (media, crop) {
        (Some(media), Some(crop)) => Some(media.intersect(&crop).unwrap_or(media)),
        (Some(media), None) => Some(media),
        (None, Some(crop)) => Some(LETTER.intersect(&crop).unwrap_or(LETTER)),
        (None, None) => None,
    }
}

/// 沿 `/Parent` 链读可继承矩形属性；**首个良构 array 胜出**（PDF 规范）。
/// 链上限 32 层（防环形引用）；`get_dictionary` 失败（对象缺失/非字典）即止。
fn find_inherited_box(doc: &Document, page_id: lopdf::ObjectId, key: &[u8]) -> Option<PageBox> {
    let mut id = page_id;
    for _ in 0..32 {
        let dict = doc.get_dictionary(id).ok()?;
        if let Ok(obj) = dict.get(key) {
            let array = match obj {
                Object::Array(array) => Some(array),
                Object::Reference(r) => match doc.get_object(*r) {
                    Ok(Object::Array(array)) => Some(array),
                    _ => None,
                },
                _ => None,
            };
            if let Some(array) = array {
                // 元素本身可为间接引用；与渲染器一致读前 4 个数、忽略多余项。
                let values: Vec<f32> = array
                    .iter()
                    .filter_map(|v| match v {
                        Object::Reference(r) => {
                            doc.get_object(*r).ok().and_then(get_number)
                        }
                        _ => get_number(v),
                    })
                    .collect();
                if values.len() >= 4
                    && let Some(b) = PageBox::from_corners(values[0], values[1], values[2], values[3])
                {
                    return Some(b);
                }
            }
        }
        match dict.get(b"Parent") {
            Ok(Object::Reference(parent)) => id = *parent,
            _ => return None,
        }
    }
    None
}

/// lopdf 对象 → 数（Integer/Real；与 pdf-inspector `get_number` 同口径，
/// 不接 String/Bool——规范里 box 元素只能是数值）。
fn get_number(obj: &Object) -> Option<f32> {
    match obj {
        Object::Integer(i) => Some(*i as f32),
        Object::Real(r) => Some(*r),
        _ => None,
    }
}

/// 读全文档每页的可见页框（键 = 1 基页号，与 `TextItem::page` 同序）。
/// 吃字节切片（调用方用 `open_pdf_bytes` 的 mmap 载体喂入，避免整文档双份堆）。
///
/// 任何失败（解析失败 / 页树损坏）→ 空 map：调用方所有页回落
/// `ContentExtent`，行为与 #11b-v2 之前逐字节一致。页框读不出是"少一个 bbox"，
/// 不是转换失败——宁缺勿造，也不许因它毁掉整篇输出。
pub(crate) fn page_visible_boxes(bytes: &[u8]) -> BTreeMap<u32, PageBox> {
    let Ok(doc) = Document::load_mem(bytes) else {
        return BTreeMap::new();
    };
    doc.get_pages()
        .into_iter()
        .filter_map(|(num, id)| visible_page_box(&doc, id).map(|b| (num, b)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{dictionary, Dictionary};

    /// 单页文档：`page_keys` 落在页字典，`parent_keys` 落在 `/Pages` 节点
    /// （复刻 pdf-inspector 测试的构造方式）。
    fn doc_with_boxes(
        page_keys: &[(&str, [i64; 4])],
        parent_keys: &[(&str, [i64; 4])],
    ) -> (Document, lopdf::ObjectId) {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let mut page = dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
        };
        for (key, v) in page_keys {
            page.set(*key, boxed(*v));
        }
        let page_id = doc.add_object(page);
        let mut pages = dictionary! {
            "Type" => "Pages",
            "Count" => 1,
            "Kids" => vec![Object::Reference(page_id)],
        };
        for (key, v) in parent_keys {
            pages.set(*key, boxed(*v));
        }
        doc.objects.insert(pages_id, pages.into());
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => Object::Reference(pages_id),
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));
        (doc, page_id)
    }

    fn boxed(v: [i64; 4]) -> Object {
        Object::Array(v.iter().map(|&n| n.into()).collect())
    }

    fn pb(x0: f32, y0: f32, x1: f32, y1: f32) -> PageBox {
        PageBox { x0, y0, x1, y1 }
    }

    #[test]
    fn media_box_alone_is_the_visible_box() {
        let (doc, id) = doc_with_boxes(&[("MediaBox", [0, 0, 612, 792])], &[]);
        assert_eq!(visible_page_box(&doc, id), Some(pb(0.0, 0.0, 612.0, 792.0)));
    }

    #[test]
    fn crop_box_inside_media_box_wins() {
        let (doc, id) = doc_with_boxes(
            &[("MediaBox", [0, 0, 400, 500]), ("CropBox", [50, 60, 350, 460])],
            &[],
        );
        assert_eq!(visible_page_box(&doc, id), Some(pb(50.0, 60.0, 350.0, 460.0)));
    }

    #[test]
    fn crop_box_is_intersected_with_offset_media_box() {
        // 实测形态：MediaBox 原点非零、CropBox 下探出界 → 渲染器显示交集。
        let (doc, id) = doc_with_boxes(
            &[("MediaBox", [36, 36, 648, 819]), ("CropBox", [36, 0, 648, 783])],
            &[],
        );
        assert_eq!(visible_page_box(&doc, id), Some(pb(36.0, 36.0, 648.0, 783.0)));
    }

    #[test]
    fn disjoint_crop_box_falls_back_to_media_box() {
        let (doc, id) = doc_with_boxes(
            &[("MediaBox", [0, 0, 400, 500]), ("CropBox", [900, 900, 950, 950])],
            &[],
        );
        assert_eq!(visible_page_box(&doc, id), Some(pb(0.0, 0.0, 400.0, 500.0)));
    }

    #[test]
    fn reversed_corners_are_normalized() {
        let (doc, id) = doc_with_boxes(&[("MediaBox", [612, 792, 0, 0])], &[]);
        assert_eq!(visible_page_box(&doc, id), Some(pb(0.0, 0.0, 612.0, 792.0)));
    }

    #[test]
    fn boxes_are_inherited_from_the_page_tree() {
        let (doc, id) = doc_with_boxes(
            &[],
            &[("MediaBox", [0, 0, 400, 500]), ("CropBox", [50, 60, 350, 460])],
        );
        assert_eq!(visible_page_box(&doc, id), Some(pb(50.0, 60.0, 350.0, 460.0)));
        // 页级 MediaBox 覆盖继承值；CropBox 仍继承。
        let (doc, id) = doc_with_boxes(
            &[("MediaBox", [0, 0, 300, 300])],
            &[("MediaBox", [0, 0, 400, 500]), ("CropBox", [50, 60, 350, 460])],
        );
        assert_eq!(visible_page_box(&doc, id), Some(pb(50.0, 60.0, 300.0, 300.0)));
    }

    #[test]
    fn indirect_box_operands_are_resolved() {
        // `/CropBox [50 60 350 7 0 R]`、`7 0 obj 460`：不解引用会只剩 3 数、
        // 整个 CropBox 被跳过回落 MediaBox。
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let top = doc.add_object(Object::Real(460.0));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => boxed([0, 0, 400, 500]),
            "CropBox" => Object::Array(vec![
                50.into(),
                60.into(),
                350.into(),
                Object::Reference(top),
            ]),
        });
        doc.objects.insert(
            pages_id,
            dictionary! {
                "Type" => "Pages",
                "Count" => 1,
                "Kids" => vec![Object::Reference(page_id)],
            }
            .into(),
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => Object::Reference(pages_id),
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));
        assert_eq!(visible_page_box(&doc, page_id), Some(pb(50.0, 60.0, 350.0, 460.0)));
    }

    #[test]
    fn crop_box_without_media_box_is_measured_against_letter() {
        let (doc, id) = doc_with_boxes(&[("CropBox", [100, 100, 700, 900])], &[]);
        assert_eq!(visible_page_box(&doc, id), Some(pb(100.0, 100.0, 612.0, 792.0)));
    }

    #[test]
    fn degenerate_and_missing_boxes_are_none() {
        let (doc, id) = doc_with_boxes(&[("MediaBox", [0, 0, 0, 792])], &[]);
        assert_eq!(visible_page_box(&doc, id), None, "零面积框当没有");
        let (doc, id) = doc_with_boxes(&[], &[]);
        assert_eq!(visible_page_box(&doc, id), None);
    }

    #[test]
    fn malformed_array_falls_through_to_parent() {
        // 页级 MediaBox 是 3 数残框 → 不胜出、继续向上，继承 `/Pages` 的良构框。
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => Object::Array(vec![0.into(), 0.into(), 400.into()]),
        });
        doc.objects.insert(
            pages_id,
            dictionary! {
                "Type" => "Pages",
                "Count" => 1,
                "Kids" => vec![Object::Reference(page_id)],
                "MediaBox" => boxed([0, 0, 500, 600]),
            }
            .into(),
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => Object::Reference(pages_id),
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));
        assert_eq!(visible_page_box(&doc, page_id), Some(pb(0.0, 0.0, 500.0, 600.0)));
    }

    #[test]
    fn non_numeric_key_is_skipped_not_fatal() {
        // MediaBox 存在但不是 array（字符串）→ 视同无此 key，回 LETTER None 规则。
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => "not-an-array",
        });
        doc.objects.insert(
            pages_id,
            dictionary! {
                "Type" => "Pages",
                "Count" => 1,
                "Kids" => vec![Object::Reference(page_id)],
            }
            .into(),
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => Object::Reference(pages_id),
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));
        assert_eq!(visible_page_box(&doc, page_id), None);
        assert!(Dictionary::get(&dictionary! {"a" => 1}, b"a").is_ok(), "lopdf API 形状守卫");
    }

    #[test]
    fn unloadable_bytes_yield_empty_map() {
        // 解析失败 → 空 map（调用方全页回落 ContentExtent，零行为变化）。
        assert!(page_visible_boxes(b"not a pdf at all").is_empty());
    }
}
