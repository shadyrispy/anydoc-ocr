//! anydoc-ocr：办公文档（含图片型 PDF/OFD）转 Markdown 的库与 CLI。
mod error;
pub use error::Result;

pub mod convert;
pub mod detect;
pub mod gfm_adapter;
pub mod models;
pub mod ofd;
pub mod pdf;
pub mod reading_order;
pub mod timing;

pub use convert::{convert_to_markdown, ConvertOptions};
pub use detect::DocKind;
pub use models::OcrTier;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
