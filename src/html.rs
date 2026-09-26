//! HTML → Markdown（Step 5 flash 档格式覆盖对齐）。
//!
//! MinerU flash 对 `.html` 走 DocVortex 原生整本解析；anydoc 0.2.4 无 HTML 前端，
//! 故本库直补：`htmd`（html5ever 解析，纯 Rust）输出 GFM。
//! 结构化要点：`<table>` → GFM 管道表、标题层级 → `#`、代码块 → fence——
//! 与 MinerU HTML 通道语义等价（均为结构直通，无 OCR/版面模型参与）。
use std::path::Path;

use crate::error::{ConvertError, ErrorKind, Stage};
use crate::Result;

/// HTML 文档转 Markdown。解析失败按 `Malformed` 归类（IO 失败按 `Io`）。
pub(crate) fn convert_html(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).map_err(|e| ConvertError::io(Stage::Convert, e))?;
    let html = match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => {
            // 非 UTF-8：GBK 等遗留编码 html5ever 也不会更好，lossy 交给解析器定性
            String::from_utf8_lossy(e.as_bytes()).into_owned()
        }
    };
    htmd::convert(&html).map_err(|e| {
        ConvertError::new(ErrorKind::Malformed, Stage::Convert, format!("HTML 解析失败: {e}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmphtml(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("anydoc_html_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let p = dir.join(name);
        std::fs::write(&p, bytes).expect("write");
        p
    }

    /// 结构直通断言：标题/列表/管道表/粗体均按 GFM 出。
    #[test]
    fn html_to_gfm() {
        let p = tmphtml(
            "a.html",
            b"<h1>\xe6\xa0\x87\xe9\xa2\x98</h1><table><tr><th>A</th><th>B</th></tr>\
              <tr><td>1</td><td>2</td></tr></table><p><strong>x</strong></p>",
        );
        let md = convert_html(&p).expect("convert");
        assert!(md.contains("# 标题"), "{md}");
        assert!(md.contains("| A") && md.contains("2"), "{md}");
        assert!(md.contains("**x**"), "{md}");
    }

    #[test]
    fn missing_file_is_io() {
        let e = convert_html(Path::new("/nonexistent/anydoc_html_test.html")).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Io);
    }
}
