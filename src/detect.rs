//! 格式检测：PDF / OFD / 其他（anydoc 支持的格式）
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocKind {
    Pdf,
    Ofd,
    Other,
}

/// 魔数检测：`%PDF` → Pdf；`PK\x03\x04` 且 zip 内含 `OFD.xml` → Ofd；其余 → Other。
///
/// P0-3：文件打不开/读不到返回 `Err(io::Error)`——此前静默归 `Other` 会被误判为
/// "格式不支持"而走 anydoc 兜底，丢失真实 IO 错误分类（不存在/无权限等）。
/// zip 打不开不算 IO 错误（docx 等合法 zip 但非 OFD），归 `Other`。
pub fn detect(path: &Path) -> std::io::Result<DocKind> {
    let mut f = std::fs::File::open(path)?;
    let mut head = [0u8; 4];
    f.read_exact(&mut head)?;
    if &head == b"%PDF" {
        return Ok(DocKind::Pdf);
    }
    // F5：复用已打开的句柄（seek 回 0），避免对同一路径二次 open。
    if &head == b"PK\x03\x04" && is_ofd_zip(&mut f) {
        return Ok(DocKind::Ofd);
    }
    Ok(DocKind::Other)
}

fn is_ofd_zip(f: &mut std::fs::File) -> bool {
    if f.seek(SeekFrom::Start(0)).is_err() {
        return false;
    }
    let Ok(z) = zip::ZipArchive::new(f) else {
        return false;
    };
    z.file_names()
        .any(|n| n.eq_ignore_ascii_case("OFD.xml") || n.ends_with("/OFD.xml"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// 每测试独立子目录（并行测试互不干扰），返回 (dir, 文件路径)。
    fn tmpfile(test: &str, name: &str, bytes: &[u8]) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("anydoc_detect_{test}_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let p = dir.join(name);
        let mut f = std::fs::File::create(&p).expect("create");
        f.write_all(bytes).expect("write");
        (dir, p)
    }

    /// P2 魔数表：PDF 头 → Pdf（只认首 4 字节）。
    #[test]
    fn magic_pdf() {
        let (dir, p) = tmpfile("pdf", "a.pdf", b"%PDF-1.7\nbinary...");
        assert_eq!(detect(&p).unwrap(), DocKind::Pdf);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// P2 魔数表：PK zip + 根级 OFD.xml → Ofd。
    #[test]
    fn magic_ofd_root_entry() {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            z.start_file("OFD.xml", zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(b"<ofd/>").unwrap();
            z.finish().unwrap();
        }
        let (dir, p) = tmpfile("ofd", "a.ofd", buf.get_ref());
        assert_eq!(detect(&p).unwrap(), DocKind::Ofd);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// P2 魔数表：PK zip 但无 OFD.xml（docx）→ Other（走 anydoc 兜底）。
    #[test]
    fn magic_docx_is_other() {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            z.start_file("[Content_Types].xml", zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(b"<types/>").unwrap();
            z.finish().unwrap();
        }
        let (dir, p) = tmpfile("docx", "a.docx", buf.get_ref());
        assert_eq!(detect(&p).unwrap(), DocKind::Other);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// P2 魔数表：非 PDF 非 zip（doc 老格式 OLE 头）→ Other。
    #[test]
    fn magic_ole_is_other() {
        let (dir, p) = tmpfile("ole", "a.doc", &[0xD0, 0xCF, 0x11, 0xE0, 0, 0, 0, 0]);
        assert_eq!(detect(&p).unwrap(), DocKind::Other);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// P0-3 回归：文件不存在 → Err(io)（不再静默归 Other 走兜底）。
    #[test]
    fn missing_file_is_io_error() {
        let p = std::path::PathBuf::from("/nonexistent/anydoc_detect_test_missing.pdf");
        assert!(detect(&p).is_err());
    }

    /// P0-3 回归：文件不足 4 字节（read_exact 失败）→ Err(io)。
    #[test]
    fn short_file_is_io_error() {
        let (dir, p) = tmpfile("short", "empty.bin", b"%P");
        assert!(detect(&p).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
