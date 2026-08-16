//! 双栏 PDF 文本层抽取顺序集成测试（真实样例）。
//!
//! 使用 tests/real_samples/上海公报2025第1期.pdf（52 页、含文本层、正文双栏）。
//! 该 PDF 文本层经 pdf-inspector 抽取 + reading_order 排序后，双栏页面应输出为
//! "左栏全部内容在前、右栏全部内容在后"，而非旧行为下左右栏逐行交错。
//!
//! 运行前提（必须设置环境变量，否则运行时库加载失败）：
//! ```text
//! export ORT_LIB_LOCATION=/workspace/anydoc-ocr/third_party/ort/x64/onnxruntime-linux-x64-1.20.1/lib
//! export ORT_INCLUDE_LOCATION=/workspace/anydoc-ocr/third_party/ort/x64/onnxruntime-linux-x64-1.20.1/include
//! export ORT_PREFER_DYNAMIC_LINK=1
//! export PDFIUM_LIB_DIR=/workspace/anydoc-ocr/third_party/pdfium/x64/lib
//! export OAR_HOME=/root/.oar
//! export LD_LIBRARY_PATH=/workspace/anydoc-ocr/third_party/ort/x64/onnxruntime-linux-x64-1.20.1/lib:/workspace/anydoc-ocr/third_party/pdfium/x64/lib
//! ```
//!
//! 该 PDF 含文本层，走文本抽取路径，运行应很快（<5s），不会触发 OCR。
//!
//! 显式运行：`cargo test --test two_column -- --ignored --nocapture`

use std::path::Path;

use anydoc_ocr::ConvertOptions;
use anydoc_ocr::ForceFlags;
use anydoc_ocr::convert_to_markdown;

const SAMPLE: &str = "tests/real_samples/上海公报2025第1期.pdf";

#[test]
#[ignore]
fn two_column_left_then_right_order() {
    let md =
        convert_to_markdown(Path::new(SAMPLE), &ConvertOptions::default(), ForceFlags::default())
            .expect("转换应成功");

    // 左栏锚点
    let left_head = "经研究";
    let left_body = "章予以修改和废止";
    let left_item = "一、对下列政府规章的部分条款予以修改";
    // 右栏锚点
    let right_body = "受市生态环境部门委托";
    let right_lead = "修改为";

    // 稳定子串均应存在（PDF 存在全角标点/数字变体，只断言稳定子串）
    for (label, needle) in [
        ("左栏开头", left_head),
        ("左栏正文", left_body),
        ("左栏条目", left_item),
        ("右栏正文", right_body),
        ("右栏引出", right_lead),
    ] {
        assert!(
            md.contains(needle),
            "{label}锚点缺失: {needle:?}\n--- snippet ---\n{}\n---------------",
            snippet(&md, needle, 120)
        );
    }

    let idx_left_head = md.find(left_head).expect("left_head 已断言存在");
    let idx_left_body = md.find(left_body).expect("left_body 已断言存在");
    let idx_left_item = md.find(left_item).expect("left_item 已断言存在");
    let idx_right_body = md.find(right_body).expect("right_body 已断言存在");
    let idx_right_lead = md.find(right_lead).expect("right_lead 已断言存在");

    // 左栏内部顺序
    assert!(
        idx_left_head < idx_left_body && idx_left_body < idx_left_item,
        "左栏内部顺序异常: head={idx_left_head} body={idx_left_body} item={idx_left_item}\n--- snippet ---\n{}\n---------------",
        snippet(&md, left_head, 400)
    );

    // 核心断言：整个左栏序列必须位于右栏锚点之前（即左栏全部在右栏之前）
    assert!(
        idx_left_item < idx_right_body,
        "双栏顺序异常：右栏锚点 {right_body:?} 出现在左栏条目之前 \
         (left_item={idx_left_item}, right_body={idx_right_body})，疑似左右栏逐行交错\n--- snippet ---\n{}\n---------------",
        snippet(&md, left_body, 500)
    );
    assert!(
        idx_left_item < idx_right_lead,
        "双栏顺序异常：右栏引出 {right_lead:?} 出现在左栏条目之前 \
         (left_item={idx_left_item}, right_lead={idx_right_lead})\n--- snippet ---\n{}\n---------------",
        snippet(&md, left_body, 500)
    );
}

/// 截取 needle 附近的一小段输出，便于失败时诊断。
fn snippet(md: &str, needle: &str, radius: usize) -> String {
    match md.find(needle) {
        Some(i) => {
            let start = i.saturating_sub(radius / 2);
            let end = (i + needle.len() + radius / 2).min(md.len());
            md[start..end].to_string()
        }
        None => format!("<未找到 {needle:?}>"),
    }
}
