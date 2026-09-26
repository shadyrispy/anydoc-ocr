//! 格式检测：PDF / OFD / HTML / 分隔文本 / 其他（anydoc 兜底）
//!
//! Step 5（flash 档格式覆盖对齐）：MinerU flash 按文件后缀分流
//! （`backend/analyze.py::doc_analyze`：pdf/ofd/csv/tsv/epub/html + office 系），
//! `--tier` 对非 PDF 无效。本模块把分流表扩到与 MinerU 的 `FILE_SUFFIXES`
//! 一致：魔数可读的用魔数（HTML），无签名/嗅探易误判的按扩展名（csv/tsv），
//! 其余（office/rtf/epub/ole）继续走 anydoc 兜底——anydoc 0.2.4 已有这些前端，
//! 此前只是 `Other` 一把抓，现在归入显式 `Office` 变体保持分流可观测。
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocKind {
    Pdf,
    Ofd,
    /// HTML（`<table>` 有结构化通道；魔数或 `.html/.htm/.xhtml` 扩展名命中）
    Html,
    /// CSV/TSV 分隔文本（无内容签名，仅按扩展名）
    DelimitedText,
    /// Office 系（doc/docx/xls/xlsx/ppt/pptx/odt/ods/odp/rtf/epub）——
    /// anydoc 兜底前端已覆盖，单列变体仅为分流显式化。
    Office,
    /// 未识别 → anydoc 兜底（行为与旧 `Other` 一致）
    Other,
}

/// 魔数 + 扩展名分流（对齐 MinerU flash 的 doc_analyze 路由表）。
///
/// 判定顺序：`%PDF` 魔数 → PK zip 且含 `OFD.xml` → HTML 魔数 / html 系扩展名
/// → csv/tsv 扩展名 → office 扩展名 → `Other`。
///
/// P0-3：文件打不开/读不到返回 `Err(io::Error)`——此前静默归 `Other` 会被误判为
/// "格式不支持"而走 anydoc 兜底，丢失真实 IO 错误分类（不存在/无权限等）。
/// zip 打不开不算 IO 错误（docx 等合法 zip 但非 OFD），继续走后续分流。
pub fn detect(path: &Path) -> std::io::Result<DocKind> {
    let mut f = std::fs::File::open(path)?;
    // HTML 魔数需看头 512 字节（WHATWG：BOM/空白后以 "<!DOCTYPE html" 开头，
    // 或 "<html"/"<head"/"<body"），其余判定只用前 4 字节。
    let mut head512 = [0u8; 512];
    let n = read_up_to(&mut f, &mut head512)?;
    // P0-3 语义保持：不足 4 字节无法判定魔数，与原 `read_exact` 一致报 IO 错误。
    if n < 4 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "file too short to detect format",
        ));
    }
    let head = &head512[..n];
    if head.starts_with(b"%PDF") {
        return Ok(DocKind::Pdf);
    }
    // F5：复用已打开的句柄（seek 回 0），避免对同一路径二次 open。
    if head.starts_with(b"PK\x03\x04") && is_ofd_zip(&mut f) {
        return Ok(DocKind::Ofd);
    }
    if starts_as_html(head) {
        return Ok(DocKind::Html);
    }
    // 以下按扩展名（与 MinerU 一致：flash 后端就是按后缀分流的，csv 无签名）。
    match ext_of(path).as_str() {
        "html" | "htm" | "xhtml" => Ok(DocKind::Html),
        "csv" | "tsv" => Ok(DocKind::DelimitedText),
        "doc" | "docx" | "docm" | "xls" | "xlsx" | "xlsm" | "xlsb" | "ppt" | "pptx" | "pptm"
        | "pps" | "ppsx" | "odt" | "ods" | "odp" | "rtf" | "epub" => Ok(DocKind::Office),
        _ => Ok(DocKind::Other),
    }
}

fn read_up_to(f: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut total = 0;
    while total < buf.len() {
        match f.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(k) => total += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(total)
}

/// WHATWG 内容类型嗅探（HTML 部分，取实用子集）：跳过 BOM 与前导空白后，
/// `<!DOCTYPE html` 或 `<html`（前导 `<!--` 注释重试一次）判为 HTML。
fn starts_as_html(head: &[u8]) -> bool {
    let mut s = head;
    if let Some(rest) = s.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        s = rest;
    }
    let t = std::str::from_utf8(s).unwrap_or("");
    let t = t.trim_start();
    let t = match t.strip_prefix("<!--") {
        Some(after) => {
            // 前导注释：跳过注释体后重试一次
            match after.find("-->") {
                Some(i) => after[i + 3..].trim_start(),
                None => return false,
            }
        }
        None => t,
    };
    let lower = t.to_ascii_lowercase();
    lower.starts_with("<!doctype html") || lower.starts_with("<html")
}

fn ext_of(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default()
}

/// 已知文档扩展名全集（大小写不敏感）。`detect` 的扩展名分流与
/// `batch::is_supported_doc` 的目录遍历共用这张表，杜绝两处各抄一份而漂移。
pub fn is_known_extension(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "pdf" | "ofd" | "html" | "htm" | "xhtml" | "csv" | "tsv" | "doc" | "docx" | "docm"
            | "xls" | "xlsx" | "xlsm" | "xlsb" | "ppt" | "pptx" | "pptm" | "pps" | "ppsx"
            | "odt" | "ods" | "odp" | "rtf" | "epub"
    )
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

    /// P2 魔数表：PK zip 但无 OFD.xml（docx）→ Office（anydoc 兜底，Step5 显式化）。
    #[test]
    fn magic_docx_is_office() {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            z.start_file("[Content_Types].xml", zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(b"<types/>").unwrap();
            z.finish().unwrap();
        }
        let (dir, p) = tmpfile("docx", "a.docx", buf.get_ref());
        assert_eq!(detect(&p).unwrap(), DocKind::Office);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// P2 魔数表：非 PDF 非 zip（doc 老格式 OLE 头）→ Office（按 .doc 扩展名）。
    #[test]
    fn magic_ole_doc_is_office() {
        let (dir, p) = tmpfile("ole", "a.doc", &[0xD0, 0xCF, 0x11, 0xE0, 0, 0, 0, 0]);
        assert_eq!(detect(&p).unwrap(), DocKind::Office);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Step5：HTML 魔数（无扩展名也命中；BOM + 前导空白容忍）。
    #[test]
    fn magic_html() {
        let (dir, p) = tmpfile(
            "html",
            "blob",
            b"\xEF\xBB\xBF \n<!DOCTYPE html><html><body>x</body></html>",
        );
        assert_eq!(detect(&p).unwrap(), DocKind::Html);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Step5：`<html` 无 doctype 也算；xml 声明开头**不算**（防误伤 .xhtml 之外场景由扩展名兜底）。
    #[test]
    fn magic_html_no_doctype() {
        let (dir, p) = tmpfile("html2", "blob", b"<HTML lang=en><body>hi");
        assert_eq!(detect(&p).unwrap(), DocKind::Html);
        let _ = std::fs::remove_dir_all(dir);
        let (dir, p) = tmpfile("xml", "blob", b"<?xml version=\"1.0\"?><a/>");
        assert_eq!(detect(&p).unwrap(), DocKind::Other);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Step5：前导注释后跟 doctype（WHATWG 嗅探重试语义）。
    #[test]
    fn magic_html_after_comment() {
        let (dir, p) = tmpfile("html3", "blob", b"<!-- c --><!doctype html><p>hi");
        assert_eq!(detect(&p).unwrap(), DocKind::Html);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Step5：扩展名兜底（内容无签名，与 MinerU 按后缀分流一致）。
    #[test]
    fn ext_html_csv_tsv() {
        let (_, p) = tmpfile("ext", "page.HTM", b"whatever not really html");
        assert_eq!(detect(&p).unwrap(), DocKind::Html);
        let (_, p) = tmpfile("ext", "t.csv", b"a,b\n1,2\n");        assert_eq!(detect(&p).unwrap(), DocKind::DelimitedText);
        let (_, p) = tmpfile("ext", "t.tsv", b"a\tb\n1\t2\n");
        assert_eq!(detect(&p).unwrap(), DocKind::DelimitedText);
        let (_, p) = tmpfile("ext", "d.rtf", b"{\\rtf1\\ansi}");
        assert_eq!(detect(&p).unwrap(), DocKind::Office);
        let (_, p) = tmpfile("ext", "e.epub", b"PK\x03\x04junk");
        assert_eq!(detect(&p).unwrap(), DocKind::Office);
        let (dir, p) = tmpfile("ext", "u.unknown", b"stuff");
        assert_eq!(detect(&p).unwrap(), DocKind::Other);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Step5 回归护栏：CSV 内容绝不能被 HTML 魔数抢走（无 `<` 开头时扩展名判定）。
    #[test]
    fn csv_not_html() {
        let (dir, p) = tmpfile("csvh", "x.html", b"a,b\n1,2\n");
        // .html 扩展名兜底 → Html（内容确实是坏 HTML，交给 html 通道报错即可）
        assert_eq!(detect(&p).unwrap(), DocKind::Html);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// P0-3 回归：文件不存在 → Err(io)（不再静默归 Other 走兜底）。
    #[test]
    fn missing_file_is_io_error() {
        let p = std::path::PathBuf::from("/nonexistent/anydoc_detect_test_missing.pdf");
        assert!(detect(&p).is_err());
    }

    /// P0-3 回归：文件不足 4 字节（空/截断）→ Err(io)，与原 read_exact 语义一致。
    #[test]
    fn short_file_is_io_error() {
        let (dir, p) = tmpfile("short", "e.bin", b"");
        assert!(detect(&p).is_err());
        let (_, p) = tmpfile("short", "p.bin", b"%P");
        assert!(detect(&p).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
