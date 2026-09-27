//! 表格朝向投票（`orientation::vote_groups`）端到端回归——子进程走真实 CLI：
//!
//! - `rotated_block.pdf` 页 1 = 直立正文 + rotate(90) 表块：默认通路（无开关）
//!   即应分组摆正——正文 18 行完整 + 表为 No/Item/Qty 三列（旧行为：整页被
//!   90° 组带偏，表转置成行、正文被吞）；
//! - 页 2 全直立（单一朝向 → 投票返回 1 组 → 走旧代码路径）：用 `--pages`
//!   隔离证明分组改动对单朝向页零影响（守护"默认输出逐字节不变"契约的
//!   最直接可断言形式）；
//! - `rotated_table.pdf`（表主导页：6 行直立正文 + 21 项旋转表）：两组均
//!   ≥ MIN_GROUP_ITEMS=3 → 各自成形，正文不被表吞、表不被转置。

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_anydoc-ocr")
}

fn sample(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/samples")
        .join(name)
}

fn run(args: &[&str]) -> (Option<i32>, String, String) {
    let mut cmd = Command::new(bin());
    cmd.args(args);
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn anydoc-ocr");
    let mut out = String::new();
    let mut err = String::new();
    child.stdout.take().expect("stdout").read_to_string(&mut out).expect("read stdout");
    child.stderr.take().expect("stderr").read_to_string(&mut err).expect("read err");
    (child.wait().expect("wait").code(), out, err)
}

/// 混合朝向页：旋转表块被摆正成三列表，直立正文一组全保留。
#[test]
fn rotated_table_block_is_grouped_and_upright() {
    let pdf = sample("rotated_block.pdf");
    assert!(pdf.exists(), "缺样本 rotated_block.pdf（gen_rotated_table.py 生成）");
    let (code, out, err) = run(&[pdf.to_str().unwrap()]);
    assert_eq!(code, Some(0), "转换应成功: {err}");
    // 正文组（0°，18 行）不得被旋转组吞掉或转置
    for i in 1..=14 {
        assert!(
            out.contains(&format!("Body line {i} keeps the page upright")),
            "正文行 {i} 丢失:\n{out}"
        );
    }
    for i in 1..=4 {
        assert!(out.contains(&format!("Trailing body line {i} after")), "尾行 {i} 丢失");
    }
    // 表组（90°）摆正后应重建为 No/Item/Qty 三列横向表
    let table = out
        .lines()
        .find(|l| l.contains("<table>"))
        .expect("旋转表块应产出表格行");
    assert!(
        table.contains("<td>No</td><td>Item</td><td>Qty</td>"),
        "表头未按摆正后的列序重建:\n{table}"
    );
    assert!(
        table.contains("<td>part-01 desc</td>") && table.contains("<td>18</td>"),
        "数据单元格错位（转置回归）:\n{table}"
    );
    // 旧 bug 形态：90° 阅读序下每个视觉行变成纵向串——防回归显式拒绝
    assert!(
        !table.contains("<td>No</td></tr>"),
        "表头串被当单列输出（转置未修）:\n{table}"
    );
}

/// 单一朝向页（页 2）：投票 → 1 组 → 旧路径，逐字节不受分组改动影响。
/// `--pages` 隔离两页对比：页 2 的输出必须恰好等于全量输出中的页 2 部分。
#[test]
fn uniform_page_output_is_untouched_by_grouping() {
    let pdf = sample("rotated_block.pdf");
    let (_, full, _) = run(&[pdf.to_str().unwrap()]);
    let (_, p2, _) = run(&[pdf.to_str().unwrap(), "--pages", "2"]);
    // 全量输出中页 2 的六行必须原样连续出现（无表插队、无重排）
    let mut expected = String::new();
    for i in 1..=6 {
        expected.push_str(&format!(
            "Upright only page two line {i} has no rotated content at all.\n"
        ));
    }
    assert!(full.contains(expected.trim_end()), "页 2 内容在全量输出中形态异常:\n{full}");
    assert_eq!(p2.trim_end(), expected.trim_end(), "单朝向页经 --pages 隔离后输出漂移");
}

/// 表主导页（rotated_table.pdf）：正文与表各自成形，互不吞并。
#[test]
fn turned_frame_strays_split_from_rotated_table() {
    let pdf = sample("rotated_table.pdf");
    let (code, out, err) = run(&[pdf.to_str().unwrap()]);
    assert_eq!(code, Some(0), "转换应成功: {err}");
    for i in 1..=6 {
        assert!(
            out.contains(&format!("Body line {i} keeps the page frame upright")),
            "直立杂散正文行 {i} 丢失:\n{out}"
        );
    }
    let table = out.lines().find(|l| l.contains("<table>")).expect("应产出表格");
    assert!(
        table.contains("<td>No</td><td>Item</td><td>Qty</td>"),
        "摆正后表头列序异常:\n{table}"
    );
    assert!(table.contains("<td>part-07 description</td>"), "末行数据丢失:\n{table}");
}
