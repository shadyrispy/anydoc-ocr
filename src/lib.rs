//! anydoc-ocr：办公文档（含图片型 PDF/OFD）转 Markdown 的库与 CLI。
//!
//! 公开 API 面：入口 [`convert_to_markdown`]、配置 [`ConvertRequest`]、文档类型 [`DocKind`]、
//! OCR 档位 [`OcrLayout`]、错误类型 [`ConvertError`]，以及 [`VERSION`]。
mod error;

pub mod batch;
pub mod convert;
pub mod detect;
pub(crate) mod docir;
pub(crate) mod fallback;
pub(crate) mod gfm_adapter;
pub(crate) mod html;
pub(crate) mod limits;
pub mod models;
pub mod ocr_engine; // 对外高级 API：OcrEngine 单例（build/predict/clear_cache），README 已文档化
pub(crate) mod ofd;
pub(crate) mod pagerange;
pub(crate) mod pdf;
pub(crate) mod pipeline;
pub mod quality;
pub(crate) mod reading_order;
pub(crate) mod region;
pub(crate) mod table_grid;
pub(crate) mod text_health;
pub(crate) mod timing;

pub use convert::{ConvertRequest, ForceFlags, OcrConfig, ParallelConfig, RenderConfig, convert_to_markdown};
pub use detect::DocKind;
pub use error::{ConvertError, ErrorKind, Result, Stage};
pub use models::OcrTier;

/// `--dpi` 合法性校验（审计 #9：NaN/Inf 或超出 [50, 400] → `Unsupported` Err）。
/// 库转换入口内部各通道已自带同语义闸；导出此函数供 CLI（及绑定层）提前拒绝，
/// 避免非法 dpi 跑完 stdin 落盘等前置工作才报错。
pub fn validate_render_dpi(dpi: f32) -> Result<()> {
    limits::validate_dpi(dpi)
}

/// `--pages` 语法早期校验：仅查与页数无关的非法语法（token 形态、倒序区间）；
/// rN 换算与空集判定需要文档总页数，由库侧 `route_pdf` 终审。
/// `None` / 空白 / `all` = 不限制 → 恒 Ok。CLI 用（同 [`validate_render_dpi`]）。
pub fn validate_page_range_syntax(raw: Option<&str>) -> Result<()> {
    match raw.filter(|r| !pagerange::is_unrestricted(r)) {
        None => Ok(()),
        Some(r) => pagerange::check_syntax(r),
    }
}

/// 有界读取 stdin（审计 #8）：累计超过 `ANYDOC_MAX_INPUT_BYTES`（默认 200 MiB）
/// 立即断读并返回 `ResourceLimit` Err，杜绝无界读把内存打满。CLI `resolve_stdin`
/// 用；导出供绑定层实现自己的 `-` 入口时复用同一闸语义。
pub fn read_stdin_bounded() -> Result<Vec<u8>> {
    limits::read_stdin_bounded()
}

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
