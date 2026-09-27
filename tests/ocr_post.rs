//! OCR 后处理层（印章识别 / `ANYDOC_TABLE_FILL`）端到端回归。
//!
//! 子进程走真实 CLI（同 `pages_rich_text.rs`）：edition-2024 下 `set_var` 不安全，
//! 且这些开关进 `EngineKey`，必须在**进程启动前**定格才有意义。
//!
//! 前提（缺一即**跳过**，不当失败——CI 无模型时不该红）：
//! ```text
//! export OAR_HOME=/root/.oar
//! export ANYDOC_MODEL_DIR=/data/models/mineru-ocr   # 需含 seal_ppocrv4_det.onnx
//! export LD_LIBRARY_PATH=<ort>/lib:<pdfium>/lib
//! ```
//!
//! 断言口径（#10b 起印章**默认开**，见 `ocr_post::seal_on`）：
//! 1. 默认输出含 `【印章】专用章` 一行，**且只多这一行**（除该行外与关闭态逐字节一致）；
//! 2. `ANYDOC_NO_SEAL_OCR` 关闭后**逐字节不含** `【印章】`——守护"可一键退回旧行为"；
//! 3. 老变量 `ANYDOC_SEAL_OCR=1` 是**别名**（仍为开），绝不反向翻转；
//! 4. 第 2 页永不长出印章行；文本精确度**不做**断言（rec 对章内红字召回有限，环排
//!    公司名当前按弧行判定显式跳过，见 BACKLOG.md #5a），只断言结构与增益位置。

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// OCR 懒初始化在 4 核沙箱下并发加载模型有 SIGTRAP 竞态（同 hybrid.rs）。
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_anydoc-ocr")
}

fn sample(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/samples")
        .join(name)
}

/// 返回 (OAR_HOME, ANYDOC_MODEL_DIR)；模型资产不齐 → None（调用方跳过）。
fn model_env() -> Option<Vec<(String, String)>> {
    let home = std::env::var("OAR_HOME").ok().filter(|v| !v.is_empty())?;
    let dir = std::env::var("ANYDOC_MODEL_DIR").ok().filter(|v| !v.is_empty())?;
    // 印章检测器必须可得（任一兼容名）
    let has_seal = ["seal_ppocrv4_det.onnx", "seal_PP-OCRv4_det_infer.onnx", "pp-ocrv4_mobile_seal_det.onnx"]
        .iter()
        .any(|n| PathBuf::from(&dir).join(n).exists() || PathBuf::from(&home).join(n).exists());
    if !has_seal {
        return None;
    }
    Some(vec![("OAR_HOME".into(), home), ("ANYDOC_MODEL_DIR".into(), dir)])
}

/// 印章/回捞相关开关（父 shell 若已 export 会污染基线，故每次运行都显式清除）。
/// `ANYDOC_SEAL_OCR` 是 #10b 前的老名，现仅为别名，仍需清除以免污染。
const GATES: [&str; 3] = ["ANYDOC_SEAL_OCR", "ANYDOC_NO_SEAL_OCR", "ANYDOC_TABLE_FILL"];

/// `on` = 要置为 "1" 的开关集合；模型环境由 `base` 显式传入。
fn run(pdf: &Path, base: &[(String, String)], on: &[&str]) -> (Option<i32>, String, String) {
    let mut cmd = Command::new(bin());
    cmd.arg(pdf).arg("--dpi").arg("150");
    for k in GATES {
        cmd.env_remove(k);
    }
    for (k, v) in base {
        cmd.env(k, v);
    }
    for k in on {
        cmd.env(k, "1");
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn anydoc-ocr");
    let mut out = String::new();
    let mut err = String::new();
    child.stdout.take().expect("stdout").read_to_string(&mut out).expect("read stdout");
    child.stderr.take().expect("stderr").read_to_string(&mut err).expect("read stderr");
    (child.wait().expect("wait").code(), out, err)
}

/// 逐字节对比：默认（开）相对 `ANYDOC_NO_SEAL_OCR`（关）**只**多出印章行。
#[test]
fn seal_default_on_adds_only_the_seal_line() {
    let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(base) = model_env() else {
        eprintln!("[ocr_post] 缺 OAR_HOME/ANYDOC_MODEL_DIR 或印章模型，跳过");
        return;
    };
    let pdf = sample("seal_scan.pdf");
    assert!(pdf.exists(), "缺样本 seal_scan.pdf（gen_seal_doc.py 生成）");

    // 默认：#10b 起印章识别**开启**
    let (code, on, err_on) = run(&pdf, &base, &[]);
    assert_eq!(code, Some(0), "默认运行应成功: {err_on}");
    // 显式关闭：退回 #10b 前的行为
    let (code, off, err_off) = run(&pdf, &base, &["ANYDOC_NO_SEAL_OCR"]);
    assert_eq!(code, Some(0), "关闭后应成功: {err_off}");

    assert!(!off.contains("【印章】"), "ANYDOC_NO_SEAL_OCR 必须完全关掉印章行:\n{off}");
    assert!(on.contains("【印章】"), "默认应识别出印章行:\n{on}");
    assert_eq!(on.matches("【印章】").count(), 1, "一枚章只应出一行（嵌套框去重失效？）:\n{on}");
    assert!(on.contains("专用章"), "章底直排行应可读:\n{on}");
    // 印章行只属于第 1 页（第 2 页无章 → 永不长出）
    assert_eq!(on[..on.find("【印章】").unwrap()].matches("Page two").count(), 0, "印章行位置错页");

    // 除该行外，开相对关不得有任何其他差异
    fn strip(s: &str) -> Vec<&str> {
        s.lines().filter(|l| !l.starts_with("【印章】")).collect()
    }
    assert_eq!(strip(&off), strip(&on), "后处理改动了非印章内容（字节契约破坏）");
}

/// 老变量 `ANYDOC_SEAL_OCR=1` 在 #10b 后必须是**无害别名**：不得把默认的开翻转成关。
#[test]
fn legacy_seal_var_is_a_no_op_alias() {
    let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(base) = model_env() else {
        eprintln!("[ocr_post] 缺模型环境，跳过");
        return;
    };
    let pdf = sample("seal_scan.pdf");
    if !pdf.exists() {
        return;
    }
    let (_, plain, _) = run(&pdf, &base, &[]);
    let (code, legacy, err) = run(&pdf, &base, &["ANYDOC_SEAL_OCR"]);
    assert_eq!(code, Some(0), "老变量不应致错: {err}");
    assert_eq!(plain, legacy, "ANYDOC_SEAL_OCR=1 必须与默认（开）逐字节一致");

    // 两个同时给出：以关闭为准（NO_ 赢），且不 panic
    let (code, both, err) = run(&pdf, &base, &["ANYDOC_SEAL_OCR", "ANYDOC_NO_SEAL_OCR"]);
    assert_eq!(code, Some(0), "同时设置应成功并以关闭为准: {err}");
    assert!(!both.contains("【印章】"), "同时设置时 NO_ 必须赢:\n{both}");
}

/// 表格回捞在**无空格**的现网样本上必须零改动（当前实网表格通路无
/// "有墨却为空"的格可造，故这里守的是"不乱改"这一半契约）。
/// 回捞本体（网格 → td 映射）由 `src/ocr_post.rs` 单测覆盖。
#[test]
fn table_fill_on_is_byte_identical_when_nothing_to_fill() {
    let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(base) = model_env() else {
        eprintln!("[ocr_post] 缺模型环境，跳过");
        return;
    };
    for name in ["real_table.pdf", "image_table.pdf", "seal_scan.pdf"] {
        let pdf = sample(name);
        if !pdf.exists() {
            continue;
        }
        let (_, off, _) = run(&pdf, &base, &[]);
        let (code, on, err) = run(&pdf, &base, &["ANYDOC_TABLE_FILL"]);
        assert_eq!(code, Some(0), "{name} 开启回捞应成功: {err}");
        assert_eq!(off, on, "{name}: 无可回捞空格时输出必须逐字节一致");
    }
}
