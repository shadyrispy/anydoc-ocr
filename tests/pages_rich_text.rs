//! MinerU 借鉴三件套回归（子进程走真实 CLI，避开 edition-2024 `set_var` 竞争，
//! 环境变量闸也生效）：
//!
//! - `--pages`（语法对齐 docvortex `page_range.py`）：选页装配、rN 倒数、
//!   倒序/非法/空集报错、非 PDF 拒绝、目录早拒、`all` 不触发拒绝；
//! - `ANYDOC_MAX_PAGES` 页数闸（对齐 MinerU `max_pages_per_file=1000`）：
//!   报错发生在 OCR/渲染之前；
//! - `ANYDOC_RICH_TEXT` **已废弃**（#6 决策 (c)）：设与不设输出逐字节相同，
//!   命中时 stderr 打一次废弃说明。样本 `rich_text.pdf` 保留——默认通路仍要
//!   一个含 bold/italic 字体文字层的文档来钉"纯文本里不得冒出样式标记"。

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// OCR 懒初始化在 4 核沙箱下并发加载模型有 SIGTRAP 竞态（同 hybrid.rs），
/// 需要 OCR 的用例经此串行。
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_anydoc-ocr")
}

fn sample(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/samples")
        .join(name)
}

struct Run {
    code: Option<i32>,
    out: String,
    err: String,
}

fn run(args: &[&str], envs: &[(&str, &str)]) -> Run {
    let mut cmd = Command::new(bin());
    cmd.args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn anydoc-ocr");
    let mut out = String::new();
    let mut err = String::new();
    child.stdout.take().expect("stdout").read_to_string(&mut out).expect("read stdout");
    child.stderr.take().expect("stderr").read_to_string(&mut err).expect("read stderr");
    let code = child.wait().expect("wait").code();
    Run { code, out, err }
}

fn tmp_md(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("anydoc_pages_{}_{}.md", tag, std::process::id()))
}

// ── --pages：正向选页 ──

/// 文字层多页文档：`--pages 1,3-4` 只装配所选页，未选页既不进输出也不补 OCR。
#[test]
fn pages_selects_only_requested_text_pages() {
    let pdf = sample("multipage.pdf");
    let r = run(&[pdf.to_str().unwrap(), "--pages", "1,3-4"], &[]);
    assert_eq!(r.code, Some(0), "应成功: {}", r.err);
    assert!(r.out.contains("第1页第1行") && r.out.contains("第3页第1行") && r.out.contains("第4页第1行"));
    assert!(!r.out.contains("第2页"), "未选页不得进输出:\n{}", &r.out[..r.out.len().min(200)]);
    assert!(!r.out.contains("第5页"), "区间外语义失效");
}

/// rN 倒数记法：8 页文档 `--pages r2-r1` = 末两页（7/8）。
#[test]
fn pages_reverse_notation_from_document_end() {
    let pdf = sample("multipage.pdf");
    let r = run(&[pdf.to_str().unwrap(), "--pages", "r2-r1"], &[]);
    assert_eq!(r.code, Some(0), "应成功: {}", r.err);
    assert!(r.out.contains("第7页第1行") && r.out.contains("第8页第1行"));
    assert!(!r.out.contains("第6页"), "rN 换算错误:\n{}", &r.out[..r.out.len().min(200)]);
}

/// 混合文档只选文字层页（1,3）→ 纯文字层路径，扫描页（2）不进 OCR 补页集合
/// （守护 LayerHit.select 交集：否则 got==want 校验必失败 NeedsOcr）。
#[test]
fn pages_on_hybrid_excludes_unselected_scanned_page() {
    let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let pdf = sample("mixed_scan.pdf");
    let r = run(&[pdf.to_str().unwrap(), "--pages", "1,3", "--dpi", "100"], &[]);
    assert_eq!(r.code, Some(0), "应成功: {}", r.err);
    assert_eq!(r.out.matches("Text PDF smoke test 123").count(), 2, "所选文字层页丢失");
    assert!(!r.out.contains("OCR Test 123"), "未选扫描页不得进输出/OCR:\n{}", r.out);
}

/// 混合文档只选扫描页（2）→ 该页按所选集进 OCR，文字层页不出现在输出。
#[test]
fn pages_on_hybrid_can_ocr_only_selected_scanned_page() {
    let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let pdf = sample("mixed_scan.pdf");
    let r = run(&[pdf.to_str().unwrap(), "--pages", "2", "--dpi", "100"], &[]);
    assert_eq!(r.code, Some(0), "应成功: {}", r.err);
    assert!(r.out.contains("OCR Test 123"), "所选扫描页未 OCR:\n{}", r.out);
    assert!(!r.out.contains("Text PDF smoke test 123"), "未选文字层页泄漏进输出");
}

// ── --pages：错误路径 ──

/// 非法语法 / 倒序区间 / 空交集 → `unsupported` 且点名语法，不落 OCR。
#[test]
fn pages_invalid_inputs_rejected() {
    let pdf = sample("multipage.pdf");
    for (bad, want) in [
        ("5-2", "不支持倒序区间"),
        ("x", "非法页码范围"),
        ("12", "选页为空"),
    ] {
        let r = run(&[pdf.to_str().unwrap(), "--pages", bad, "-o", "/dev/null"], &[]);
        assert_eq!(r.code, Some(1), "--pages {bad} 应失败");
        assert!(
            r.err.contains("unsupported") && r.err.contains(want),
            "--pages {bad} stderr 应含 {want}: {}",
            r.err
        );
    }
}

/// 非 PDF 文档显式给页 → 显式拒绝（MinerU 对非 PDF 报 page_range_invalid 同口径）。
#[test]
fn pages_on_non_pdf_rejected() {
    let ofd = sample("text.ofd");
    let r = run(&[ofd.to_str().unwrap(), "--pages", "1", "-o", "/dev/null"], &[]);
    assert_eq!(r.code, Some(1), "OFD + --pages 应失败");
    assert!(r.err.contains("仅支持 PDF"), "stderr 应点名 PDF 限定: {}", r.err);
}

/// `--pages all` = 未提供（不触发非 PDF 拒绝，OFD 正常转换）。
#[test]
fn pages_all_is_no_op_on_non_pdf() {
    let ofd = sample("text.ofd");
    let out = tmp_md("all_ofd");
    let r = run(&[ofd.to_str().unwrap(), "--pages", "all", "-o", out.to_str().unwrap()], &[]);
    let _ = std::fs::remove_file(&out);
    assert_eq!(r.code, Some(0), "--pages all 不得改变非 PDF 行为: {}", r.err);
}

/// 目录输入 + 显式页 → CLI 早拒（MinerU "Only works for single PDFs" 同口径）；
/// `--pages all` 仍放行批处理。
#[test]
fn pages_on_directory_input_rejected() {
    let dir = std::env::temp_dir().join(format!("anydoc_pages_dir_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::copy(sample("text.pdf"), dir.join("t.pdf")).expect("copy");
    let out = dir.join("md");
    let r = run(&[dir.to_str().unwrap(), "--pages", "1", "-o", out.to_str().unwrap()], &[]);
    assert_eq!(r.code, Some(1), "目录 + --pages 应失败");
    assert!(r.err.contains("仅支持单文档输入"), "stderr 应点名目录限制: {}", r.err);
    let r = run(&[dir.to_str().unwrap(), "--pages", "all", "-o", out.to_str().unwrap()], &[]);
    assert_eq!(r.code, Some(0), "--pages all 应放行批处理: {}", r.err);
    let _ = std::fs::remove_dir_all(&dir);
}

// ── ANYDOC_MAX_PAGES 页数闸 ──

/// 超限 PDF → `resourceLimit`，报错在 classify 阶段（OCR/渲染之前）。
#[test]
fn page_gate_rejects_before_ocr() {
    let pdf = sample("multipage.pdf");
    let r = run(&[pdf.to_str().unwrap(), "-o", "/dev/null"], &[("ANYDOC_MAX_PAGES", "2")]);
    assert_eq!(r.code, Some(1), "8>2 页应失败");
    assert!(
        r.err.contains("resource limit exceeded") && r.err.contains("ANYDOC_MAX_PAGES"),
        "stderr 应点名页数闸与可调变量: {}",
        r.err
    );
    assert!(!r.err.contains("[ocr]"), "闸应在 OCR 之前: {}", r.err);
    // 调高即放行（闸不误伤正常规模）
    let r = run(&[pdf.to_str().unwrap(), "-o", "/dev/null"], &[("ANYDOC_MAX_PAGES", "10")]);
    assert_eq!(r.code, Some(0), "limit=10 应通过: {}", r.err);
}

// ── ANYDOC_RICH_TEXT 已废弃（#6 决策 (c)）──

/// 废弃契约的两半：**行为半**——设与不设 stdout 逐字节相同，且无论哪种情况都
/// 不出行内样式标记（旧路径开启时会产出 `**Bold lead-in text**` / `*Italic styled
/// text*` / `## **1. General Rules**`，这三处形状是废弃最敏感的残留点）；
/// **反馈半**——命中时 stderr 打一次废弃说明，绝不静默（老脚本设了值却没任何
/// 反应是最难查的一类问题）。
///
/// 样本用 `rich_text.pdf`（`gen_rich_text.py`）：它是仓内唯一带 bold/italic
/// 字体证据的文字层 PDF，废弃后仍用于钉"producer 不再注入样式字面量"。
#[test]
fn rich_text_env_is_a_no_op_with_notice() {
    let pdf = sample("rich_text.pdf");
    assert!(pdf.exists(), "缺样本 rich_text.pdf（gen_rich_text.py 生成）");

    let off = run(&[pdf.to_str().unwrap()], &[]);
    assert_eq!(off.code, Some(0), "默认路径应成功: {}", off.err);
    assert!(off.out.contains("## 1. General Rules"), "标题前缀行为不变:\n{}", off.out);
    assert!(off.out.contains("Bold lead-in text"), "正文不丢");
    assert!(!off.out.contains("**"), "默认不得注入样式标记:\n{}", off.out);
    assert!(
        !off.err.contains("ANYDOC_RICH_TEXT"),
        "未设置该变量时不得凭空告警: {}",
        off.err
    );

    let legacy = run(&[pdf.to_str().unwrap()], &[("ANYDOC_RICH_TEXT", "1")]);
    assert_eq!(legacy.code, Some(0), "设了废弃变量仍应成功: {}", legacy.err);
    assert_eq!(
        legacy.out, off.out,
        "废弃变量必须不改变任何行为：设了它之后的 stdout 应与默认逐字节相同"
    );
    assert!(!legacy.out.contains("**"), "开启语义已移除，不得产出样式标记:\n{}", legacy.out);
    assert!(
        legacy.err.contains("ANYDOC_RICH_TEXT") && legacy.err.contains("已废弃"),
        "stderr 应含废弃说明: {}",
        legacy.err
    );

    // 判据同旧开关：存在即命中，不限值——`=0` 也告警（否则用户会以为值写错了）。
    let zero = run(&[pdf.to_str().unwrap()], &[("ANYDOC_RICH_TEXT", "0")]);
    assert!(zero.err.contains("已废弃"), "=0 同样应告警: {}", zero.err);

    // 文档面：废弃口径必须出现在 `--help`（MINERU_ENGINE_HELP 的 after_help 段）。
    // 此前该变量**从未**在 `--help` 里出现过，故这一断言是新增契约，不是回归网。
    let help = run(&["--help"], &[]);
    assert_eq!(help.code, Some(0), "--help 应成功: {}", help.err);
    let text = format!("{}{}", help.out, help.err);
    assert!(
        text.contains("ANYDOC_RICH_TEXT") && text.contains("已废弃"),
        "--help 应写明 ANYDOC_RICH_TEXT 已废弃:\n{text}"
    );
}

/// 告警**每进程一次**：批处理目录逐文件都过 `route_doc`，不记忆就会每行刷屏。
#[test]
fn rich_text_notice_prints_once_per_process() {
    let dir = std::env::temp_dir().join(format!("anydoc_rich_dep_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::copy(sample("rich_text.pdf"), dir.join("a.pdf")).expect("copy");
    std::fs::copy(sample("rich_text.pdf"), dir.join("b.pdf")).expect("copy");
    let out = dir.join("md");
    let r = run(
        &[dir.to_str().unwrap(), "-o", out.to_str().unwrap()],
        &[("ANYDOC_RICH_TEXT", "1")],
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(r.code, Some(0), "批处理应成功: {}", r.err);
    assert_eq!(
        r.err.matches("ANYDOC_RICH_TEXT 已废弃").count(),
        1,
        "两文件批处理应只告警一次: {}",
        r.err
    );
}
