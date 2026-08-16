//! 格式检测：PDF / OFD / 其他（anydoc 支持的格式）
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocKind {
    Pdf,
    Ofd,
    Other,
}

pub fn detect(path: &Path) -> DocKind {
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return DocKind::Other,
    };
    let mut head = [0u8; 4];
    if f.read_exact(&mut head).is_err() {
        return DocKind::Other;
    }
    if &head == b"%PDF" {
        return DocKind::Pdf;
    }
    // F5：复用已打开的句柄（seek 回 0），避免对同一路径二次 open。
    if &head == b"PK\x03\x04" && is_ofd_zip(&mut f) {
        return DocKind::Ofd;
    }
    DocKind::Other
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
