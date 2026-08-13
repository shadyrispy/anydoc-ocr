//! 错误与结果类型
use anydoc::ConvertError;

/// 转换错误：透传 anydoc 结构化错误，支持 CLI 按类型设退出码
#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// 文档加密
    #[error("encrypted document")]
    Encrypted,
    /// 触发固定安全上限（解压/嵌套/资产字节等）
    #[error("resource limit ({limit}): {detail}")]
    ResourceLimit {
        /// 超出的限制名
        limit: &'static str,
        /// 详情
        detail: String,
    },
    /// 格式不支持
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// 结构损坏
    #[error("malformed: {detail}")]
    Malformed {
        /// 详情
        detail: String,
    },
    /// IO 错误
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// 其他错误（pdfium/ofd 等返回 anyhow 的边界）
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// ConvertError 映射：保留语义化 variant，MissingPart 归入 Unsupported
impl From<ConvertError> for Error {
    fn from(e: ConvertError) -> Self {
        match e {
            ConvertError::Encrypted => Error::Encrypted,
            ConvertError::ResourceLimit { limit, detail } => {
                Error::ResourceLimit { limit, detail }
            }
            ConvertError::Unsupported(s) => Error::Unsupported(s),
            ConvertError::Malformed { detail, .. } => Error::Malformed { detail },
            ConvertError::MissingPart { part } => Error::Unsupported(format!("missing: {part}")),
            ConvertError::Io(e) => Error::Io(e),
            _ => Error::Other(anyhow::anyhow!("unknown anydoc error")),
        }
    }
}

impl Error {
    /// CLI 退出码：encrypted=2 / resourceLimit=3 / unsupported=4 / 其他=1
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::Encrypted => 2,
            Error::ResourceLimit { .. } => 3,
            Error::Unsupported(_) => 4,
            _ => 1,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    /// ConvertError 标 #[non_exhaustive]，外部无法直接构造 variant。
    /// 通过真实输入触发各错误路径，验证 From 映射与 exit_code。
    /// Unsupported: 未识别的字节 + 不指定格式
    #[test]
    fn from_convert_error_unsupported() {
        let e = anydoc::to_markdown_bytes(b"not a real document", None).unwrap_err();
        let e: Error = e.into();
        assert!(matches!(e, Error::Unsupported(_)));
        assert_eq!(e.exit_code(), 4);
    }

    /// Encrypted: 构造一个加密 doc 的字节难以离线复现，改为验证 exit_code 映射逻辑
    /// 通过直接断言 Error::Encrypted 的 exit_code
    #[test]
    fn exit_code_encrypted() {
        assert_eq!(Error::Encrypted.exit_code(), 2);
    }

    #[test]
    fn exit_code_resource_limit() {
        let e = Error::ResourceLimit { limit: "max_entry_bytes", detail: "x".into() };
        assert_eq!(e.exit_code(), 3);
    }

    #[test]
    fn exit_code_unsupported() {
        let e = Error::Unsupported("y".into());
        assert_eq!(e.exit_code(), 4);
    }

    #[test]
    fn exit_code_malformed() {
        let e = Error::Malformed { detail: "z".into() };
        assert_eq!(e.exit_code(), 1);
    }

    #[test]
    fn exit_code_io() {
        let e = Error::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "x"));
        assert_eq!(e.exit_code(), 1);
    }

    #[test]
    fn exit_code_other() {
        let e = Error::Other(anyhow::anyhow!("misc"));
        assert_eq!(e.exit_code(), 1);
    }
}
