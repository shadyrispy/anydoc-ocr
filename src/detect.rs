//! 格式检测：PDF / OFD / 其他（anydoc 支持的格式）
//!
//! PDF 与 OFD 需自有渲染/OCR 路径，本地魔数检测；其余格式交给 anydoc 的
//! `Format::from_bytes`（12 格式：doc/docx/odt/ppt/pptx/rtf/epub/xls/ods/odp/csv）。
use std::io::Read;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocKind {
    Pdf,
    Ofd,
    Other,
}

/// 检测文档类型：PDF/OFD 本地魔数优先，其余交 anydoc Format 识别。
/// 读不到头或 anydoc 不识别时返回 Other（由调用方决定是否报 Unsupported）。
pub fn detect(path: &Path) -> DocKind {
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return DocKind::Other,
    };
    let mut head = [0u8; 4];
    let n = f.read(&mut head).unwrap_or(0);
    if n >= 4 && &head == b"%PDF" {
        return DocKind::Pdf;
    }
    if n >= 4 && &head == b"PK\x03\x04" && is_ofd_zip(path) {
        return DocKind::Ofd;
    }
    // 其余格式交 anydoc：能识别即为 Other（走 anydoc 通道），不能识别也归 Other
    // （anydoc 会在 to_markdown_bytes 时报 Unsupported，错误透传到 CLI 退出码 4）
    DocKind::Other
}

fn is_ofd_zip(path: &Path) -> bool {
    let Ok(f) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(z) = zip::ZipArchive::new(f) else {
        return false;
    };
    z.file_names()
        .any(|n| n.eq_ignore_ascii_case("OFD.xml") || n.ends_with("/OFD.xml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_pdf_magic() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"%PDF-1.4\nrest").unwrap();
        assert_eq!(detect(tmp.path()), DocKind::Pdf);
    }

    #[test]
    fn detect_empty_file_is_other() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"").unwrap();
        assert_eq!(detect(tmp.path()), DocKind::Other);
    }

    #[test]
    fn detect_rtf_is_other() {
        // RTF 被 anydoc Format::from_bytes 识别为 Rtf，但 anydoc-ocr 统一归 Other
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"{\\rtf1\\ansi ...}").unwrap();
        assert_eq!(detect(tmp.path()), DocKind::Other);
    }

    #[test]
    fn detect_zip_not_ofd_is_other() {
        // 普通 zip（非 OFD）应归 Other（如 docx/xlsx，交 anydoc）
        let tmp = tempfile::NamedTempFile::new().unwrap();
        // PK 头但无 OFD.xml
        let mut buf = b"PK\x03\x04".to_vec();
        buf.extend_from_slice(&[0u8; 100]);
        std::fs::write(tmp.path(), &buf).unwrap();
        assert_eq!(detect(tmp.path()), DocKind::Other);
    }

    #[test]
    fn detect_missing_file_is_other() {
        assert_eq!(detect(std::path::Path::new("/nonexistent/path")), DocKind::Other);
    }
}
