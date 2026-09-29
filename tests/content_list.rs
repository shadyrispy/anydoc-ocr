//! #11 `--format content-list-v2` 端到端（子进程走真实 CLI，**不加载任何模型**
//! ——全程 `--text-only`，CI 绿得住）。
//!
//! 钉三件事：
//! 1. 顶层形状是 `list[list[dict]]`（按页分组）、item 键为 `{type, content[, bbox]}`；
//! 2. PDF 文字层页（分母是 `ContentExtent`）**不给 bbox**，而不是给 `[0,0,0,0]`；
//! 3. 非 DocIR 输入（office）在 content-list-v2 下显式 `unsupported`，不静默回落 md。
//!
//! schema 的逐字对齐由 `src/docir/content_list.rs` 的 12 个单测钉住（含 24 个
//! 类型名字面值），这里只管 CLI 通路。

use std::path::PathBuf;
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_anydoc-ocr")
}

fn sample(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/samples").join(name)
}

/// `--text-only` + `--format`：返回 (exit_ok, stdout, stderr)。
fn run(name: &str, format: &str) -> (bool, String, String) {
    let out = Command::new(bin())
        .args([sample(name).to_str().unwrap(), "--text-only", "--format", format])
        .output()
        .expect("run cli");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// 文字层 PDF → paragraph item，按页分组。
/// #11b-v2：text.pdf 有 MediaBox → dims=PageBoxPdfPt → **bbox 落地**（0–1000
/// 归一化、top-down 顺序）；无框页的"省略而非伪造"由 `bbox_of` 单测钉住。
/// #11c：两行近距正文合并为**一段**（bbox 跨两行），西方语境行间补空格。
#[test]
fn text_layer_pdf_projects_to_page_grouped_items_with_bbox() {
    let (ok, stdout, _) = run("text.pdf", "content-list-v2");
    assert!(ok, "stderr: {stdout}");
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("stdout 必须是合法 JSON");
    let pages = v.as_array().expect("顶层是数组（按页分组）");
    assert_eq!(pages.len(), 1, "text.pdf 单页 → 一个页槽位");
    let items = pages[0].as_array().expect("每页是 item 数组");
    assert_eq!(items.len(), 1, "#11c 两行近距正文 → 合并为一个段落 item");
    for it in items {
        assert_eq!(it["type"], "paragraph");
        // content 是对象（paragraph_content），不是裸字符串
        assert!(it["content"]["paragraph_content"].is_array());
        let bbox = it["bbox"].as_array().expect("#11b-v2 文字层页 bbox 落地");
        assert_eq!(bbox.len(), 4);
        let q: Vec<i64> = bbox.iter().map(|x| x.as_i64().unwrap()).collect();
        assert!(q.iter().all(|&c| (0..=1000).contains(&c)), "bbox 0-1000: {q:?}");
        assert!(q[1] <= q[3], "top-down：y0(上) <= y1(下): {q:?}");
    }
    let text = items[0]["content"]["paragraph_content"][0]["content"].as_str().unwrap();
    // 行间补空格（MinerU 西方语境），bbox 纵向覆盖两行
    assert_eq!(text, "Hello anydoc-ocr Text PDF smoke test 123");
    let bbox = items[0]["bbox"].as_array().unwrap();
    let y0 = bbox[1].as_i64().unwrap();
    assert!(y0 < 168, "bbox y0 应覆盖第一行顶部: {bbox:?}");
}

/// 默认 `md` 不受影响（与加 `--format` 前同一口径）。
#[test]
fn default_markdown_is_unchanged() {
    let (ok, md, _) = run("text.pdf", "md");
    assert!(ok);
    assert!(md.contains("Hello anydoc-ocr"), "markdown 通路照旧: {md}");
    assert!(md.contains("Text PDF smoke test 123"));
    assert!(!md.trim_start().starts_with('['), "默认不得吐 JSON");
}

/// OFD 文字层同样进 content_list v2（页型按 mm 记录，此处因无几何仍无 bbox）。
#[test]
fn ofd_text_layer_projects_too() {
    let (ok, stdout, stderr) = run("text.ofd", "content-list-v2");
    assert!(ok, "stderr: {stderr}");
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("合法 JSON");
    let items = v[0].as_array().expect("第一页 item 数组");
    assert!(!items.is_empty());
    assert!(items.iter().any(|it| it["type"] == "paragraph"));
}

/// 非 DocIR 输入（office 通道）在 content-list-v2 下**显式拒绝**，
/// 不静默回落 markdown（否则下游拿到的是 md 文本却被当成 JSON 解析）。
#[test]
fn non_docir_input_is_rejected_explicitly() {
    let (ok, _, stderr) = run("corrupt.docx", "content-list-v2");
    assert!(!ok, "office 输入不得成功");
    let msg = format!("{stderr}{}", "");
    assert!(
        msg.contains("content_list v2") && msg.contains("unsupported"),
        "错误信息要点明是 content_list v2 不支持该输入: {msg}"
    );
}
