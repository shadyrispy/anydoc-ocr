//! 错误与结果类型
//!
//! ADR-0006：全库统一 `anydoc::ConvertError`，移除 anyhow。
//! - PDF `PdfError` → `ConvertError`：复用 anydoc `formats/pdf.rs` map_error 语义
//! - OFD `OfdError` → `ConvertError`：按语义分类
//! - 运行时错误（ORT/pdfium）→ `ConvertError::Malformed { detail }`
pub use anydoc::ConvertError;
pub type Result<T> = std::result::Result<T, ConvertError>;

use std::io;

/// PDF `PdfError` → `ConvertError`（ADR-0006 映射表）。
///
/// 与 anydoc `formats/pdf.rs:40-48` 的 map_error 对齐：
/// `Encrypted → Encrypted`、`NotAPdf/InvalidStructure/Parse → Malformed`、`Io → Io`。
pub fn from_pdf_error(e: pdf_inspector::PdfError) -> ConvertError {
    use pdf_inspector::PdfError;
    match e {
        PdfError::Encrypted => ConvertError::Encrypted,
        PdfError::NotAPdf(d) => ConvertError::Malformed {
            part: None,
            detail: format!("not a PDF: {d}"),
        },
        PdfError::InvalidStructure => ConvertError::Malformed {
            part: None,
            detail: "invalid PDF structure".to_string(),
        },
        PdfError::Parse(d) => ConvertError::Malformed {
            part: None,
            detail: format!("PDF parse error: {d}"),
        },
        PdfError::Io(e) => ConvertError::Io(e),
    }
}

/// OFD `OfdError` → `ConvertError`（ADR-0006 映射表）。
///
/// ofd-core 0.3.0 不支持加密识别（已核验），OFD 加密文档会以
/// `Structure`/`Xml` 失败，统一归 `Malformed`。
pub fn from_ofd_error(e: ofd_core::OfdError) -> ConvertError {
    use ofd_core::OfdError;
    match e {
        OfdError::Io(e) => ConvertError::Io(e),
        OfdError::Zip(e) => ConvertError::Malformed {
            part: None,
            detail: format!("OFD zip 容器损坏: {e}"),
        },
        OfdError::EntryNotFound(part) => ConvertError::MissingPart { part },
        OfdError::Structure(d) => ConvertError::Malformed {
            part: None,
            detail: format!("OFD 结构错误: {d}"),
        },
        OfdError::Xml(e) => ConvertError::Malformed {
            part: None,
            detail: format!("OFD XML 解析失败: {e}"),
        },
        OfdError::BasicType { ty, value, reason } => ConvertError::Malformed {
            part: None,
            detail: format!("OFD 基础类型 {ty} 解析失败（值 {value:?}）: {reason}"),
        },
        OfdError::Image(e) => ConvertError::Malformed {
            part: None,
            detail: format!("OFD 图片编解码失败: {e}"),
        },
        OfdError::Render(d) => ConvertError::Malformed {
            part: None,
            detail: format!("OFD 渲染失败: {d}"),
        },
    }
}

/// 运行时错误（ORT/pdfium/渲染失败）→ `ConvertError::Malformed`（ADR-0006 §3）。
///
/// 运行时错误非文档本身问题，但 `ConvertError` 是 anydoc external type 不能加变体，
/// 统一归 `Malformed + detail`。调用方按 `code()` 返 `malformed` 处理，detail 区分。
pub fn runtime(part: Option<&str>, detail: impl Into<String>) -> ConvertError {
    ConvertError::Malformed {
        part: part.map(|s| s.to_string()),
        detail: detail.into(),
    }
}

/// `io::Error` 便利构造（anydoc 已 impl `From<io::Error>`，这里仅转发）。
pub fn from_io(e: io::Error) -> ConvertError {
    ConvertError::Io(e)
}
