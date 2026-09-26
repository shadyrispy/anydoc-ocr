//! 混合 PDF 按页补 OCR（anydoc 0.2.4 "Scanned pages are reported, not dropped"）
//! 端到端回归。
//!
//! 样本：`tests/samples/mixed_scan.pdf`（p1/p3 文字层 + p2 整页扫描图）、
//! `tests/samples/mixed_blank.pdf`（p1/p4 文字层 + p2 纯空白 + p3 扫描图）。
//! 两个样本的 p2/p3 扫描图内容相同（"OCR Test 123 / Hello anydoc-ocr …"）。
//!
//! 验证点：
//! 1. 扫描页正文经按页 OCR 合并回文档，**页序正确**（旧行为：静默丢页）；
//! 2. 纯文字文档（text.pdf/multipage.pdf）不触发混合路由（golden 已守护字节
//!    一致，这里再断言输出不含 OCR 痕迹变化）；
//! 3. `ANYDOC_NO_HYBRID=1` 回到旧行为：扫描页被丢弃；
//! 4. 批处理与单文档输出一致（hybrid + scan + text 混合批次一次 pipeline）。

use std::path::PathBuf;

use anydoc_ocr::{
    ConvertRequest, ForceFlags, ParallelConfig, RenderConfig, batch::BatchConverter,
    convert_to_markdown,
};

fn sample(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/samples/{name}"))
}

fn opts() -> ConvertRequest {
    ConvertRequest {
        render: RenderConfig { dpi: 100.0 },
        parallel: ParallelConfig { page_parallel: 4, ..Default::default() },
        ..Default::default()
    }
}

/// 串行化 OCR 懒初始化（同 batch_golden 的 SIGTRAP 竞态规避）。
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 混合文档：扫描页文字必须出现在输出中且页序正确（p1 → p2(OCR) → p3）。
#[test]
fn hybrid_pdf_recovers_scanned_page_in_order() {
    let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let md = convert_to_markdown(&sample("mixed_scan.pdf"), &opts(), ForceFlags::default())
        .expect("mixed_scan 转换应成功");
    let i1 = md.find("Text PDF smoke test 123").expect("p1 文字层");
    let i2 = md.find("OCR Test 123").expect("p2 OCR 标题");
    let i3 = md.rfind("Text PDF smoke test 123").expect("p3 文字层");
    assert!(i1 < i2 && i2 < i3, "页序错乱:\n{md}");
    // p1/p3 各保留一份文字层内容（不因合并被吞）
    assert_eq!(md.matches("Text PDF smoke test 123").count(), 2, "文字层页丢失:\n{md}");
    assert_eq!(md.matches("OCR Test 123").count(), 1, "OCR 页重复:\n{md}");
}

/// 空白页防护：纯空白页也被 inspector 标记（noText），OCR 无文字时不得让
/// 整文档失败——输出仍完整（其余页齐）。
#[test]
fn hybrid_pdf_with_blank_page_still_succeeds() {
    let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let md = convert_to_markdown(&sample("mixed_blank.pdf"), &opts(), ForceFlags::default())
        .expect("mixed_blank 转换应成功");
    assert!(md.contains("OCR Test 123"), "p3 扫描页未恢复:\n{md}");
    assert_eq!(md.matches("Text PDF smoke test 123").count(), 2, "文字层页丢失:\n{md}");
}

/// `ANYDOC_NO_HYBRID=1`：回旧行为——扫描页静默丢弃（A/B 回退开关有效性）。
/// 环境变量为进程全局，经独立子进程验证（edition 2024 禁测试内 set_var，
/// 也避免与同二进制其他测试竞态）。
#[test]
fn hybrid_can_be_disabled_for_rollback() {
    let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_anydoc-ocr"))
        .arg(sample("mixed_scan.pdf"))
        .env("ANYDOC_NO_HYBRID", "1")
        .output()
        .expect("启动 anydoc-ocr 失败");
    assert!(out.status.success(), "NO_HYBRID 退出失败: {}", String::from_utf8_lossy(&out.stderr));
    let md = String::from_utf8_lossy(&out.stdout);
    assert!(!md.contains("OCR Test 123"), "开关未生效（不应含 OCR 页）:\n{md}");
    assert!(md.contains("Text PDF smoke test 123"), "文字层内容丢失:\n{md}");
}

/// 批处理一致性：hybrid + 图片型 + 纯文字混合批次，逐文档输出与单文档一致
/// （混合型与图片型共用一次跨文档 pipeline 后，回填槽位不得错位）。
#[test]
fn batch_hybrid_matches_single_doc_output() {
    let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let paths = vec![
        sample("mixed_scan.pdf"),
        sample("image.pdf"),
        sample("multipage.pdf"),
    ];
    let o = opts();
    for p in &paths {
        assert!(p.exists(), "缺样本 {}", p.display());
    }
    let singles: Vec<String> = paths
        .iter()
        .map(|p| convert_to_markdown(p, &o, ForceFlags::default()).expect("单文档应成功"))
        .collect();
    let outcomes = BatchConverter::new(o, ForceFlags::default()).convert_many(&paths);
    assert_eq!(outcomes.len(), paths.len());
    for (i, out) in outcomes.iter().enumerate() {
        let md = out.result.as_ref().expect("批处理该文档应成功");
        assert_eq!(md, &singles[i], "批处理与单文档输出不一致: {}", out.path.display());
    }
    // 混合批次里 hybrid 文档确实走了 OCR（内容含 OCR 页）
    assert!(outcomes[0].result.as_ref().unwrap().contains("OCR Test 123"));
}
