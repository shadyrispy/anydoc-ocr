//! 错误分类回归（ADR-0006 §3）：
//! 验证 PDF/OFD/anydoc 三类格式错误经 `from_pdf_error`/`from_ofd_error`/透传后
//! 映射到正确的 `ConvertError` 变体，`code()` 返稳定字符串供 main.rs 精准提示。
//!
//! 覆盖 4 类样本（tests/samples/）：
//! - `encrypted.pdf`：pikepdf R=4 加密，user 密码非空 → pdf-inspector `PdfError::Encrypted`
//!   → `from_pdf_error` → `ConvertError::Encrypted` / `code()=="encrypted"`
//! - `corrupt.pdf`：`%PDF` 魔数 + 垃圾 → pdf-inspector `InvalidStructure`/`Parse`
//!   → `ConvertError::Malformed` / `code()=="malformed"`
//! - `encrypted.docx`：msoffcrypto ECMA376 Agile 加密 → anydoc 透传
//!   `ConvertError::Encrypted` / `code()=="encrypted"`
//! - `corrupt.docx`：zip 截断 → anydoc `Malformed`（zip EOCD 找不到）
//!   / `code()=="malformed"`
//!
//! 调用 `convert_to_markdown` 走全库统一入口（detect 分流 + 错误映射），
//! 不直接调 `from_pdf_error`——端到端验证错误从源头到调用方的完整传播。

use std::path::PathBuf;

use anydoc_ocr::{convert_to_markdown, ConvertOptions};

fn sample(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/samples").join(name)
}

/// 期望转换失败且 `code()` 匹配；样本缺失则跳过（不阻塞其他环境）。
fn expect_code(path: &PathBuf, expected_code: &str, label: &str) {
    if !path.exists() {
        eprintln!("[err_cls] skip {label}: {} 缺失", path.display());
        return;
    }
    let opts = ConvertOptions::default();
    match convert_to_markdown(path, &opts) {
        Ok(md) => panic!(
            "[err_cls] {label} 应失败但成功（len={}）\n--- 输出前 200 字 ---\n{}",
            md.len(),
            md.chars().take(200).collect::<String>()
        ),
        Err(e) => {
            let got = e.code();
            assert_eq!(
                got, expected_code,
                "[err_cls] {label}: code() 期望 {expected_code} 实际 {got}（错误: {e}）"
            );
            println!("[err_cls] OK {label}: code()={got} ({e})");
        }
    }
}

#[test]
fn encrypted_pdf_classifies_as_encrypted() {
    expect_code(&sample("encrypted.pdf"), "encrypted", "encrypted.pdf");
}

#[test]
fn corrupt_pdf_classifies_as_malformed() {
    expect_code(&sample("corrupt.pdf"), "malformed", "corrupt.pdf");
}

#[test]
fn encrypted_docx_classifies_as_encrypted() {
    expect_code(&sample("encrypted.docx"), "encrypted", "encrypted.docx");
}

#[test]
fn corrupt_docx_classifies_as_malformed() {
    expect_code(&sample("corrupt.docx"), "malformed", "corrupt.docx");
}

/// PDF 加密路径在 batch 预分流阶段被拦截（ADR-0006 §5）：
/// `text_layer_markdown` 返 `Err(Encrypted)` → batch 直接标错，不送 OCR pipeline。
/// 验证 `BatchConverter::convert_many` 对加密 PDF 返回 `Encrypted` 而非走 OCR 兜底。
#[test]
fn batch_intercepts_encrypted_pdf() {
    use anydoc_ocr::batch::BatchConverter;

    let encrypted = sample("encrypted.pdf");
    if !encrypted.exists() {
        eprintln!("[err_cls] skip batch_intercepts_encrypted_pdf: encrypted.pdf 缺失");
        return;
    }
    let opts = ConvertOptions::default();
    let converter = BatchConverter::new(opts);
    let results = converter.convert_many(&[encrypted.clone()]);
    assert_eq!(results.len(), 1, "[err_cls] batch 应返回 1 个结果");
    match &results[0] {
        Ok(md) => panic!(
            "[err_cls] 加密 PDF 不应成功转换（len={}）",
            md.len()
        ),
        Err(e) => {
            assert_eq!(
                e.code(),
                "encrypted",
                "[err_cls] 加密 PDF 应 code()==encrypted，实际 {}（{e}）",
                e.code()
            );
            println!("[err_cls] OK batch_intercepts_encrypted_pdf: {e}");
        }
    }
}
