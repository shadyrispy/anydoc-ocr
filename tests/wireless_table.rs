//! #7 无线表格结构基线（结论钉子，不是精度追求）。
//!
//! 第 0 步实测结论（BACKLOG #7）：现状 `slanet_plus` 当通用兜底对无线表**已经
//! 完全正确**，而候选通路 `ANYDOC_WIRELESS_CELLS`（cells→HTML）会把结构做坏
//! （colspan 错、丢格）。故默认不开该通路。
//!
//! 本文件守的是**这个决定的两面**：
//! 1. 默认配置下无线表结构正确（防将来某次改动悄悄把现状做退，那时"开 A/B 通路
//!    当补救"会被误当成解法）；
//! 2. `ANYDOC_WIRELESS_CELLS=1` 下**有线表输出不变**（该开关只碰 wireless 分支——
//!    若哪天有线表也跟着变，说明挂错了分支，比精度问题严重）。
//!
//! 前提：`OAR_HOME` + `ANYDOC_MODEL_DIR` + 印章/表格件就位；缺则**跳过**（CI 无
//! 模型不该红）。模型环境判定同 `tests/ocr_post.rs`。

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};

static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_anydoc-ocr")
}

fn sample(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/samples")
        .join(name)
}

/// 模型就位才跑：`OAR_HOME`/`ANYDOC_MODEL_DIR` 非空 + slanet_plus 与字典可得。
fn model_env() -> Option<Vec<(String, String)>> {
    let home = std::env::var("OAR_HOME").ok().filter(|v| !v.is_empty())?;
    let dir = std::env::var("ANYDOC_MODEL_DIR").ok().filter(|v| !v.is_empty())?;
    let have = |n: &str| PathBuf::from(&dir).join(n).exists() || PathBuf::from(&home).join(n).exists();
    if !have("slanet_plus.onnx") || !have("table_structure_dict_ch.txt") {
        return None;
    }
    Some(vec![("OAR_HOME".into(), home), ("ANYDOC_MODEL_DIR".into(), dir)])
}

fn run(pdf: &PathBuf, base: &[(String, String)], extra: &[(&str, &str)]) -> (Option<i32>, String, String) {
    let mut cmd = Command::new(bin());
    cmd.arg(pdf).arg("--dpi").arg("150");
    // A/B 开关必须显式清除，否则父 shell 的 export 会把"默认"变成"开"
    cmd.env_remove("ANYDOC_WIRELESS_CELLS");
    for (k, v) in base {
        cmd.env(k, v);
    }
    for (k, v) in extra {
        cmd.env(k, v);
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn anydoc-ocr");
    let mut out = String::new();
    let mut err = String::new();
    child.stdout.take().expect("stdout").read_to_string(&mut out).expect("read stdout");
    child.stderr.take().expect("stderr").read_to_string(&mut err).expect("read stderr");
    (child.wait().expect("wait").code(), out, err)
}

/// 默认（现状通路）：无线表 + 合并单元格的结构必须**全对**。
///
/// 断言"两个 span 各在其位 + 十个格一个不丢"，不断言行数——`Merged` 跨两行时
/// 第 4 行本就只有 2 个 `<td>`，按行数断言会把正确输出判成失败。
#[test]
fn default_pipeline_keeps_wireless_spans_correct() {
    let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(base) = model_env() else {
        eprintln!("[wireless] 缺模型环境，跳过");
        return;
    };
    let pdf = sample("wireless_span.pdf");
    assert!(pdf.exists(), "缺样本 wireless_span.pdf（tests/gen_wireless_tables.py 生成）");
    let (code, md, err) = run(&pdf, &base, &[]);
    assert_eq!(code, Some(0), "默认应成功: {err}");
    assert!(md.contains("<table"), "应出表格结构:\n{md}");
    // 真值：Header1 colspan=2 / Merged rowspan=2 / 10 个格内容一个不丢
    assert!(md.contains("colspan=\"2\""), "colspan 应落在 Header1 上（=2）:\n{md}");
    assert!(!md.contains("colspan=\"3\""), "colspan 误扩到整行（A/B 通路的典型错法）:\n{md}");
    assert!(md.contains("rowspan=\"2\""), "rowspan 应为 2:\n{md}");
    assert!(!md.contains("rowspan=\"3\""), "rowspan 误扩（A/B 通路的典型错法）:\n{md}");
    for cell in ["Header1", "Header2", "Data1", "Data2", "Data3", "Data4", "Data5", "Data6", "Data7", "Merged"] {
        assert!(md.contains(cell), "单元格内容丢失: {cell}\n{md}");
    }
}

/// A/B 开关只应影响无线分支：**有线表输出逐字节不变**。
#[test]
fn wireless_cells_gate_does_not_touch_wired_tables() {
    let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(base) = model_env() else {
        eprintln!("[wireless] 缺模型环境，跳过");
        return;
    };
    let pdf = sample("wired_table.pdf");
    if !pdf.exists() {
        return;
    }
    // 该开关会加载 129MB 的 cell-det 件；**不在本地就位就不跑**（测试绝不该
    // 为了验证一个默认关闭的 A/B 开关去拉 129MB 网络资产）。
    let home = base.iter().find(|(k, _)| k == "OAR_HOME").map(|(_, v)| v.as_str()).unwrap_or("");
    let dir = base.iter().find(|(k, _)| k == "ANYDOC_MODEL_DIR").map(|(_, v)| v.as_str()).unwrap_or("");
    let cell = "rt-detr-l_wireless_table_cell_det.onnx";
    if !(PathBuf::from(dir).join(cell).exists() || PathBuf::from(home).join(cell).exists()) {
        eprintln!("[wireless] 缺 {cell}（129MB，不为其联网），跳过 A/B 侧");
        return;
    }
    let (_, off, _) = run(&pdf, &base, &[]);
    let (code, on, err) = run(&pdf, &base, &[("ANYDOC_WIRELESS_CELLS", "1")]);
    assert_eq!(code, Some(0), "开 A/B 后有线表应成功: {err}");
    assert!(err.contains("#7 A/B"), "开关生效必须打 stderr 说明（129MB 静默加载最难查）: {err}");
    assert_eq!(off, on, "有线表输出必须与开关无关（挂错分支了？）:\nOFF={off}\nON={on}");
}
