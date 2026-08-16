//! anydoc-ocr：办公文档（含图片型 PDF/OFD）转 Markdown 的库与 CLI。
//!
//! 公开 API 面：入口 [`convert_to_markdown`]、配置 [`ConvertOptions`]、文档类型 [`DocKind`]、
//! OCR 档位 [`OcrLayout`]、错误类型 [`ConvertError`]，以及 [`VERSION`]。
mod error;

pub mod batch;
pub mod convert;
pub mod detect;
pub(crate) mod emitter;
pub(crate) mod gfm_adapter;
pub mod models;
pub mod ocr_engine; // 对外高级 API：OcrEngine 单例（build/predict/clear_cache），README 已文档化
pub(crate) mod ofd;
pub(crate) mod pdf;
pub(crate) mod pipeline;
pub mod quality;
pub(crate) mod reading_order;
pub(crate) mod region;
pub(crate) mod table_grid;
pub(crate) mod text_health;
pub(crate) mod timing;

pub use convert::{ConvertOptions, ForceFlags, convert_to_markdown};
pub use detect::DocKind;
pub use error::{ConvertError, Result};
pub use models::OcrTier;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
