//! 错误与结果类型（P1.9：自有 `ConvertError`，不再透传 anydoc 外部类型）。
//!
//! ADR-0006 起全库统一错误类型；此前借用 `anydoc::ConvertError`（external type
//! 不能加变体，ORT/pdfium 运行时错误被迫归 `Malformed`）。P1.9 换为自有结构化
//! 错误 `{ kind, stage, page, detail }`：
//! - `kind`：错误分类（`code()` 返稳定字符串，调用方/绑定层按此分支）；
//! - `stage`：管线阶段（探测/提取/渲染/OCR/装配/调度/写出）；
//! - `page`：页号定位（0 基，`None` = 文档级）；
//! - `detail`：上游错误信息。
//!
//! 兜底通路（`DocKind::Other` 走 anydoc）经 `From<anydoc::ConvertError>` 转入，
//! kind 分类保留、原始 Display 存入 detail。
use std::fmt;

/// 全库统一 Result 别名。
pub type Result<T> = std::result::Result<T, ConvertError>;

/// 错误分类。`code()` 返稳定字符串（machine-readable，调用方按此分支）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// 输入不可读（路径不存在/权限不足/磁盘满）
    Io,
    /// 文档加密或带密码
    Encrypted,
    /// 文档结构不可用（损坏/解析失败）
    Malformed,
    /// 缺少转换所需的必需部件
    MissingPart,
    /// 超出固定安全限制（解压炸弹/超大文档）
    ResourceLimit,
    /// 格式不支持
    Unsupported,
    /// 运行时依赖失败（ORT/pdfium/模型），非文档本身问题。
    /// ADR-0006 §3 曾被迫归 `Malformed`（external type 不能加变体），P1.9 拆出。
    Runtime,
}

impl ErrorKind {
    /// 稳定字符串：`io` / `encrypted` / `malformed` / `missingPart` /
    /// `resourceLimit` / `unsupported` / `runtime`。
    pub fn code(self) -> &'static str {
        match self {
            ErrorKind::Io => "io",
            ErrorKind::Encrypted => "encrypted",
            ErrorKind::Malformed => "malformed",
            ErrorKind::MissingPart => "missingPart",
            ErrorKind::ResourceLimit => "resourceLimit",
            ErrorKind::Unsupported => "unsupported",
            ErrorKind::Runtime => "runtime",
        }
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ErrorKind::Io => "io error",
            ErrorKind::Encrypted => "encrypted",
            ErrorKind::Malformed => "malformed document",
            ErrorKind::MissingPart => "missing part",
            ErrorKind::ResourceLimit => "resource limit exceeded",
            ErrorKind::Unsupported => "unsupported",
            ErrorKind::Runtime => "runtime",
        })
    }
}

/// 管线阶段：错误发生处（结构化定位，替代旧的 `part: "doc 1 page 0"` 字符串）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Stage {
    /// 魔数检测（`detect`）
    Detect,
    /// 文字层/结构提取（pdf-inspector / ofd-core）
    Extract,
    /// 页面渲染（pdfium / OFD 渲染器）
    Render,
    /// OCR 推理（oar-ocr / ORT）
    Ocr,
    /// DocIR 装配与后处理 pass
    Assemble,
    /// 总调度 / anydoc 兜底通路
    Convert,
    /// 结果写出
    Output,
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Stage::Detect => "detect",
            Stage::Extract => "extract",
            Stage::Render => "render",
            Stage::Ocr => "ocr",
            Stage::Assemble => "assemble",
            Stage::Convert => "convert",
            Stage::Output => "output",
        })
    }
}

/// 结构化转换错误：`{ kind, stage, page, detail }`。
#[derive(Debug, Clone)]
pub struct ConvertError {
    /// 错误分类（`code()` 返稳定字符串）
    pub kind: ErrorKind,
    /// 管线阶段
    pub stage: Stage,
    /// 页号（0 基；`None` = 文档级）
    pub page: Option<usize>,
    /// 上游错误详情
    pub detail: String,
}

impl ConvertError {
    pub fn new(kind: ErrorKind, stage: Stage, detail: impl Into<String>) -> Self {
        Self {
            kind,
            stage,
            page: None,
            detail: detail.into(),
        }
    }

    /// 附加页号定位（0 基）。
    pub fn at_page(mut self, page: usize) -> Self {
        self.page = Some(page);
        self
    }

    /// 稳定字符串（代理 `kind.code()`）：`encrypted`/`malformed`/`runtime`/...，
    /// main 与绑定层据此给精准提示。
    pub fn code(&self) -> &'static str {
        self.kind.code()
    }
}

impl fmt::Display for ConvertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 格式：`[render:page3] runtime: {detail}`（无页号则 `[render]`）
        write!(f, "[{}", self.stage)?;
        if let Some(p) = self.page {
            write!(f, ":page{p}")?;
        }
        write!(f, "] {}: {}", self.kind, self.detail)
    }
}

impl std::error::Error for ConvertError {}

/// io::Error → `ConvertError`（`?` 自动转换用；阶段取 Convert 总调度兜底，
/// 已知阶段的调用方用 [`ConvertError::io`] 显式标注）。
impl From<std::io::Error> for ConvertError {
    fn from(e: std::io::Error) -> Self {
        ConvertError::new(ErrorKind::Io, Stage::Convert, e.to_string())
    }
}

impl ConvertError {
    /// io::Error → `ConvertError`，显式标注阶段。
    pub fn io(stage: Stage, e: std::io::Error) -> Self {
        ConvertError::new(ErrorKind::Io, stage, e.to_string())
    }
}

/// anydoc 兜底通路（`DocKind::Other`）错误转入：kind 分类保留、原始 Display
/// 存 detail（不丢 `part`/`limit` 等变体字段信息）。
impl From<anydoc::ConvertError> for ConvertError {
    fn from(e: anydoc::ConvertError) -> Self {
        let kind = match &e {
            anydoc::ConvertError::Unsupported(_) => ErrorKind::Unsupported,
            anydoc::ConvertError::Malformed { .. } => ErrorKind::Malformed,
            anydoc::ConvertError::Encrypted => ErrorKind::Encrypted,
            anydoc::ConvertError::ResourceLimit { .. } => ErrorKind::ResourceLimit,
            anydoc::ConvertError::MissingPart { .. } => ErrorKind::MissingPart,
            anydoc::ConvertError::Io(_) => ErrorKind::Io,
            // `#[non_exhaustive]`：上游未来新增变体时兜底（原始 Display 已存 detail 不丢信息）
            _ => ErrorKind::Malformed,
        };
        ConvertError::new(kind, Stage::Convert, e.to_string())
    }
}

/// 运行时错误（ORT/pdfium/模型加载失败）→ `ErrorKind::Runtime`（P1.9 拆出，
/// 不再与 `Malformed` 混淆——文档损坏提示与运行环境提示分流）。
pub fn runtime(stage: Stage, page: Option<usize>, detail: impl Into<String>) -> ConvertError {
    ConvertError {
        kind: ErrorKind::Runtime,
        stage,
        page,
        detail: detail.into(),
    }
}

/// PDF `PdfError` → `ConvertError`（提取阶段）。
pub fn from_pdf_error(e: pdf_inspector::PdfError) -> ConvertError {
    use pdf_inspector::PdfError;
    match e {
        PdfError::Encrypted => ConvertError::new(ErrorKind::Encrypted, Stage::Extract, "PDF 加密"),
        PdfError::NotAPdf(d) => {
            ConvertError::new(ErrorKind::Malformed, Stage::Extract, format!("not a PDF: {d}"))
        }
        PdfError::InvalidStructure => ConvertError::new(
            ErrorKind::Malformed,
            Stage::Extract,
            "invalid PDF structure",
        ),
        PdfError::Parse(d) => ConvertError::new(
            ErrorKind::Malformed,
            Stage::Extract,
            format!("PDF parse error: {d}"),
        ),
        PdfError::Io(e) => ConvertError::io(Stage::Extract, e),
    }
}

/// OFD `OfdError` → `ConvertError`（提取阶段）。
///
/// ofd-core 0.3.0 不支持加密识别（已核验），OFD 加密文档会以
/// `Structure`/`Xml` 失败，统一归 `Malformed`。
pub fn from_ofd_error(e: ofd_core::OfdError) -> ConvertError {
    use ofd_core::OfdError;
    match e {
        OfdError::Io(e) => ConvertError::io(Stage::Extract, e),
        OfdError::Zip(e) => ConvertError::new(
            ErrorKind::Malformed,
            Stage::Extract,
            format!("OFD zip 容器损坏: {e}"),
        ),
        OfdError::EntryNotFound(part) => ConvertError::new(
            ErrorKind::MissingPart,
            Stage::Extract,
            format!("OFD 缺少部件: {part}"),
        ),
        OfdError::Structure(d) => ConvertError::new(
            ErrorKind::Malformed,
            Stage::Extract,
            format!("OFD 结构错误: {d}"),
        ),
        OfdError::Xml(e) => ConvertError::new(
            ErrorKind::Malformed,
            Stage::Extract,
            format!("OFD XML 解析失败: {e}"),
        ),
        OfdError::BasicType { ty, value, reason } => ConvertError::new(
            ErrorKind::Malformed,
            Stage::Extract,
            format!("OFD 基础类型 {ty} 解析失败（值 {value:?}）: {reason}"),
        ),
        OfdError::Image(e) => ConvertError::new(
            ErrorKind::Malformed,
            Stage::Extract,
            format!("OFD 图片编解码失败: {e}"),
        ),
        OfdError::Render(d) => ConvertError::new(
            ErrorKind::Malformed,
            Stage::Render,
            format!("OFD 渲染失败: {d}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    //! P1.9 语义契约：kind 分类 + stage/page 定位 + code() 稳定字符串 +
    //! anydoc 兜底转换不丢分类。

    use super::*;
    use pdf_inspector::PdfError;

    #[test]
    fn error_kind_codes_are_stable() {
        assert_eq!(ErrorKind::Io.code(), "io");
        assert_eq!(ErrorKind::Encrypted.code(), "encrypted");
        assert_eq!(ErrorKind::Malformed.code(), "malformed");
        assert_eq!(ErrorKind::MissingPart.code(), "missingPart");
        assert_eq!(ErrorKind::ResourceLimit.code(), "resourceLimit");
        assert_eq!(ErrorKind::Unsupported.code(), "unsupported");
        assert_eq!(ErrorKind::Runtime.code(), "runtime");
    }

    #[test]
    fn from_pdf_error_maps_each_variant() {
        // Encrypted → kind Encrypted / stage Extract
        let e = from_pdf_error(PdfError::Encrypted);
        assert_eq!(e.kind, ErrorKind::Encrypted);
        assert_eq!(e.stage, Stage::Extract);
        assert_eq!(e.code(), "encrypted");

        // NotAPdf / InvalidStructure / Parse → Malformed
        assert_eq!(
            from_pdf_error(PdfError::NotAPdf("x".into())).kind,
            ErrorKind::Malformed
        );
        assert_eq!(
            from_pdf_error(PdfError::InvalidStructure).kind,
            ErrorKind::Malformed
        );
        assert_eq!(
            from_pdf_error(PdfError::Parse("x".into())).kind,
            ErrorKind::Malformed
        );

        // Io → kind Io
        let io_err = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert_eq!(from_pdf_error(PdfError::Io(io_err)).kind, ErrorKind::Io);
        assert_eq!(from_pdf_error(PdfError::Io(std::io::Error::from(
            std::io::ErrorKind::NotFound
        ))).code(), "io");
    }

    #[test]
    fn from_ofd_error_maps_each_variant() {
        use ofd_core::OfdError;
        // EntryNotFound → MissingPart
        assert_eq!(
            from_ofd_error(OfdError::EntryNotFound("OFD.xml".into())).kind,
            ErrorKind::MissingPart
        );
        // Zip / Structure / Xml → Malformed
        assert_eq!(
            from_ofd_error(OfdError::Structure("bad".into())).kind,
            ErrorKind::Malformed
        );
        // Io → Io
        assert_eq!(
            from_ofd_error(OfdError::Io(std::io::Error::from(
                std::io::ErrorKind::PermissionDenied
            )))
            .kind,
            ErrorKind::Io
        );
        // Render → Malformed + stage Render
        let e = from_ofd_error(OfdError::Render("boom".into()));
        assert_eq!(e.kind, ErrorKind::Malformed);
        assert_eq!(e.stage, Stage::Render);
    }

    #[test]
    fn runtime_error_is_distinct_kind_with_stage_and_page() {
        let e = runtime(Stage::Render, Some(3), "渲染失败: gpu oom");
        assert_eq!(e.kind, ErrorKind::Runtime);
        assert_eq!(e.code(), "runtime");
        assert_eq!(e.stage, Stage::Render);
        assert_eq!(e.page, Some(3));
        // Display 含 stage/page 定位与 detail
        let s = e.to_string();
        assert!(s.contains("[render:page3]"), "display: {s}");
        assert!(s.contains("渲染失败"), "display: {s}");
    }

    #[test]
    fn from_anydoc_error_preserves_kind_and_detail() {
        let e: ConvertError = anydoc::ConvertError::Encrypted.into();
        assert_eq!(e.kind, ErrorKind::Encrypted);
        assert_eq!(e.stage, Stage::Convert);

        let e: ConvertError = anydoc::ConvertError::MissingPart {
            part: "word/document.xml".into(),
        }
        .into();
        assert_eq!(e.kind, ErrorKind::MissingPart);
        assert!(e.detail.contains("word/document.xml"), "detail: {}", e.detail);

        let e: ConvertError = anydoc::ConvertError::ResourceLimit {
            limit: "max_entry_bytes",
            detail: "entry too large".into(),
        }
        .into();
        assert_eq!(e.kind, ErrorKind::ResourceLimit);
        assert_eq!(e.code(), "resourceLimit");
    }

    #[test]
    fn io_error_conversion_sets_kind() {
        let e: ConvertError = std::io::Error::from(std::io::ErrorKind::NotFound).into();
        assert_eq!(e.kind, ErrorKind::Io);
        assert_eq!(e.code(), "io");
        // 显式阶段标注
        let e2 = ConvertError::io(Stage::Output, std::io::Error::other("disk full"));
        assert_eq!(e2.stage, Stage::Output);
        assert_eq!(e2.kind, ErrorKind::Io);
    }
}
