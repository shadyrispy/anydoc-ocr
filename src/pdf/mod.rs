//! PDF 通道：文字型走 pdf-inspector 提取 + 自建阅读顺序还原；图片型走 OCR 管线。
//!
//! 文字型不再用 `anydoc::to_markdown()`：其内部是 pdf-inspector 的"既有版式路径"
//! （朴素 y/x 排序），双列页面逐行交错，且 pdf-inspector 内置 reading_order 是
//! 图像锚定、证据门控的局部列流处理，纯文字双列页永不触发。改为直接调
//! `extract_text_with_positions()` 拿带坐标 TextItem → 复用公共模块
//! `reading_order`（与 OCR 通路同一算法）还原阅读顺序。
//!
//! 关键：pdf-inspector 的 `group_into_lines` 会把同一行的左右两列合并成一行
//! （先于列检测糊掉列边界），所以这里在检测到双列后**按 gutter 拆行**，把每行
//! 拆成列内独立行，再交给 `order_text_regions` 做左列全→右列全。
//!
//! 文字层提取管线（`text_layer_markdown` 及其辅助函数/常量/测试）已拆至
//! 独立文件 [`text_layer`](self::text_layer)。
//!
//! T2-B/R1/R3：文字层通路对"含表格页"回退 OCR。不再单靠 pdf-inspector 的
//! `pages_with_tables`（弱：漏首页标题块、偶误报），改为可疑集 = 文字层启发式
//! （>=3 行各自拆成 >=3 个 x 分离段，双列正文每行仅 2 段不误报）；首/末页
//! 曾无条件入集，Ticket B 已移除（无证据召回，代价是整页渲染+版面 OCR），末页
//! 表格改由 `probe_last_page_table` 兜底。可疑页整文档懒渲染一次后批量跑版面
//! OCR（用 `opts.ocr_layout`，默认 Doc 含 table 类，能识别封面/版权栏等），
//! 以 `LayoutElementType::Table` 确认后才输出 `<table>` HTML（MinerU 对齐：
//! 表格只出自识别模型，不来自文字层），未确认页回落文字层；页序混排保序，
//! OCR 失败回落该页文字层。`--pdf-force-ocr` 仍为整文档 OCR。
//!
//! T2-B：跨页重复的"页面家具/水印"（页眉/页脚/居中/斜向水印）在文字层按
//! "同文本 + 同归一化位置跨页重复达阈值"剔除，避免污染阅读顺序。
use std::path::Path;

use crate::timing::StageTimer;
use crate::{ConvertOptions, Result, gfm_adapter};

pub mod render;
mod text_layer;
use text_layer::text_layer_markdown;

pub fn convert_pdf(path: &Path, opts: &ConvertOptions) -> Result<String> {
    let mut t = StageTimer::new();
    // 文字型：pdf-inspector 提取 + 自建阅读顺序；非文字型/失败回退 OCR。
    // --pdf-force-ocr 强制把文字型当图片渲染后 OCR（图片型校准）。
    if !opts.pdf_force_ocr
        && let Some(md) = text_layer_markdown(path, opts)?
    {
        return Ok(md);
    }
    // 图片型：PDFium 渲染 + oar-ocr OCR（全量渲：空索引 = 渲所有页）
    // DPI 默认 100（可由 --dpi 调整）。DPI 200→100：像素量降 75%，实测 上海公报52p
    // 148.5s→100.0s(-33%)，内容恢复率零损失(99.83%)；80 起脚注/小字开始漏检。
    let images = render::render_pdf_pages(path, opts.dpi, &[])?;
    t.stage("render");
    let pages =
        crate::ocr_engine::ocr_images(images, opts.ocr_tier, opts.ocr_layout, opts.threads)?;
    t.stage("ocr");
    let md = gfm_adapter::structure_results_to_gfm(&pages);
    t.stage("gfm");
    Ok(md)
}
