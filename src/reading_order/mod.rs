//! 文本区域的阅读顺序还原（双列/多列感知排序）。
//!
//! OCR 通路（`gfm_adapter`）与文字层通路（`pdf/text_layer.rs`、`ofd/`）共用同一
//! 排序算法：把所有区域按 x 中心排序，取**最大间隙**切分出列——双列页面的列间
//! gutter 正是最大间隙；单栏页面的最大间隙很小（只是列内相邻行的 x 抖动），不
//! 触发切分。每列内按 y 排序、列间从左到右。
//!
//! P1.8 拆子模块（纯移动，不改行为），按职责分四个文件：
//! - [`columns`]：区域驱动的列检测与排序（`order_text_regions` / `detect_column_split`），
//!   三级降级链的末级兜底；
//! - [`blocks`]：ADR-0009 块驱动三级降级链（order_index → RegionBlock → 区域兜底）
//!   与坐标归一化（`page_scale` / `norm_membership`）；
//! - [`lines`]：行级后处理（连字符合并、全角归一化）与段落合并；
//! - [`title`]：MinerU 式标题级别推断（编号启发式）。
//!
//! 关键点：列检测用**所有**文本区域（含宽条目）。早先版本把跨度>60% 页宽的
//! 元素当作"整宽"剔除，但公报目录里左列的长标题（如"上海市人民政府办公厅
//! 关于转发市交通委制订的《上海港口基础设施维护管理办法》的通知"）跨度就达
//! ~68% 页宽，被误删后列间隙被糊掉、分栏失败。因此只把**真正跨整页**（x 同时
//! 贴近左右边距）的元素（页眉/页脚/通栏标题）剔除为 header/footer，左列长条目
//! 按其 x 中心自然归入左列。
//!
//! 无清晰间隙（单栏或无法切分）时退化为纯 y 排序，兼容单栏文档与边缘情况。
//!
//! `regions`: [`Region`]（`x_min/x_max/y_min/y_max/文本`）。

mod blocks;
mod columns;
mod lines;
mod list;
mod title;
mod vertical;

pub use blocks::order_structure;
pub use columns::{detect_column_split, order_text_regions};
pub use lines::postprocess_lines;
pub use list::is_isolated_marker;
pub use title::title_level;

pub(crate) use blocks::{norm_membership, page_scale};
