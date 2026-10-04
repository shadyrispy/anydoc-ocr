//! #8 表格方向矫正端到端回归。
//!
//! **N6 实测后翻默认**：`ANYDOC_TABLE_ORI` 现在默认**开**，`=0` 才关（见
//! [`models::table_ori_wanted`](../src/models.rs) 的 A/B 数据表）。故本文件的
//! "ON" = 不传该env（默认档）、"OFF" = `ANYDOC_TABLE_ORI=0`。
//!
//! 子进程走真实 CLI（同 `wireless_table.rs`）：该设置进 `EngineKey`，必须在进程
//! 启动前定格；edition-2024 下 `set_var` 不安全。
//!
//! 前提（缺一即**跳过**，不当失败）：`OAR_HOME`/`ANYDOC_MODEL_DIR` 之一里有
//! `pp-lcnet_x1_0_doc_ori.onnx`。翻默认后它已是 `MINERU_ASSETS` 必需件（走
//! auto-download），但 CI 上不该为了跑这三条契约去下 6.8MB——同
//! `wireless_table.rs` 对 129MB 件的处理口径。
//!
//! 守的两件事（顺序即重要性）：
//! 1. **默认档不动正常表**——直立表 + 仓内全部既有表样本在 ON/OFF 下**逐字节
//!    相同**。这是 #8 唯一的硬契约：它是"新增第二道方向信号"，不是"重做表格
//!    识别"。实测（2026-09-27）：`table_upright` / `wired_table` /
//!    `wireless_span` / `wireless_simple` / `image_table.pdf` / `rotated_table`
//!    六件全等。
//! 2. **默认档确实修旋转表**——`table_rot90.pdf` 在 OFF 下是转置残局（3 行 × 5 列、
//!    单元格被劈成 "C" / "herry" / "B" / "nana"），ON 下网格形状正确（每行 3 格、
//!    ≥4 行、表头 Item/Quantity 就位）。
//!    注意 ON 的 rec 仍不完美（"Price"→"rice"、"10"→"1"），故这里**只断言网格形状
//!    与表头存在**，不逐格断言文本——逐格断言会把 rec 精度也钉进来，那是 #7 的债。

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

/// 返回要透传给子进程的环境变量；模型资产不齐 → None（调用方跳过）。
fn model_env() -> Option<Vec<(String, String)>> {
    let home = std::env::var("OAR_HOME").ok().filter(|v| !v.is_empty())?;
    let dir = std::env::var("ANYDOC_MODEL_DIR").ok().filter(|v| !v.is_empty()).unwrap_or_default();
    let have = |d: &str| !d.is_empty() && Path::new(d).join("pp-lcnet_x1_0_doc_ori.onnx").exists();
    if !have(&dir) && !Path::new(&home).join("pp-lcnet_x1_0_doc_ori.onnx").exists() {
        return None;
    }
    let mut v = vec![("OAR_HOME".into(), home)];
    if !dir.is_empty() {
        v.push(("ANYDOC_MODEL_DIR".into(), dir));
    }
    Some(v)
}

/// 会改变表格通路输出的开关：每次运行显式清除，防父 shell export 污染基线。
const GATES: [&str; 4] = [
    "ANYDOC_TABLE_ORI",
    "ANYDOC_WIRELESS_CELLS",
    "ANYDOC_TABLE_FILL",
    "ANYDOC_NO_SEAL_OCR",
];

fn run(pdf: &Path, base: &[(String, String)], extra: &[&str]) -> (Option<i32>, String, String) {
    let mut cmd = Command::new(bin());
    cmd.arg(pdf).arg("--dpi").arg("150");
    for k in GATES {
        cmd.env_remove(k);
    }
    for (k, v) in base {
        cmd.env(k, v);
    }
    // N6 翻默认后 `ANYDOC_TABLE_ORI` **默认开**（见 `models::table_ori_wanted`），
    // 故此helper 的 `extra` 是「**关**某个 gate」而非「开」：传 `("ANYDOC_TABLE_ORI", "0")`
    // 得到关闭态，不传即默认开启态。`ANYDOC_TABLE_ORI=1` 与不传同义。
    for k in extra {
        cmd.env(k, "0");
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn");
    let mut out = String::new();
    let mut err = String::new();
    child.stdout.take().expect("stdout").read_to_string(&mut out).expect("read stdout");
    child.stderr.take().expect("stderr").read_to_string(&mut err).expect("read stderr");
    (child.wait().expect("wait").code(), out, err)
}

/// 每行 `<td>` 数（忽略 `<th>`；我们表格统一 td）。
fn cells_per_row(md: &str) -> Vec<usize> {
    md.split("<tr>")
        .skip(1)
        .map(|seg| match seg.find("</tr>") {
            Some(end) => seg[..end].matches("<td").count(),
            None => seg.matches("<td").count(),
        })
        .collect()
}

/// 行数（`<tr>` 出现次数）。
fn row_count(md: &str) -> usize {
    md.matches("<tr>").count()
}

/// 契约 1：开关**不动**正常表——直立件与全部既有表样本逐字节相同。
#[test]
fn gate_leaves_upright_and_existing_tables_byte_identical() {
    let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(base) = model_env() else {
        eprintln!("[table_orientation] 缺 pp-lcnet_x1_0_doc_ori.onnx，跳过");
        return;
    };
    for name in [
        "table_upright.pdf",
        "wired_table.pdf",
        "wireless_span.pdf",
        "wireless_simple.pdf",
        "image_table.pdf",
        // 文字层件的表（表主导页）：证明开关不会波及非 OCR 通路
        "rotated_table.pdf",
    ] {
        let pdf = sample(name);
        assert!(pdf.exists(), "缺样本 {name}");
        let (co, on, eon) = run(&pdf, &base, &[]);
        assert_eq!(co, Some(0), "{name} 默认档应成功: {eon}");
        let (cn, off, eoff) = run(&pdf, &base, &["ANYDOC_TABLE_ORI"]);
        assert_eq!(cn, Some(0), "{name} 显式关闭应成功: {eoff}");
        assert_eq!(on, off, "{name} 关掉表格方向矫正后输出变了——该页没有旋转表，第二道方向信号不该改变结果");
    }
}

/// 生效要有痕迹：默认档运行时 stderr 打一行说明（6.8MB 的加载不能无声）。
#[test]
fn gate_announces_itself_when_enabled() {
    let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(base) = model_env() else {
        eprintln!("[table_orientation] 缺 pp-lcnet_x1_0_doc_ori.onnx，跳过");
        return;
    };
    let pdf = sample("table_upright.pdf");
    let (_, _, err_on) = run(&pdf, &base, &[]);
    assert!(err_on.contains("#8"), "默认档应在 stderr 说明: {err_on}");
    let (_, _, err_off) = run(&pdf, &base, &["ANYDOC_TABLE_ORI"]);
    assert!(!err_off.contains("#8"), "显式关闭时不该出现 #8 说明");
}

/// 契约 2：旋转表在 ON 下网格形状正确，OFF 下是转置残局。
#[test]
fn gate_uprights_rotated_table_grid() {
    let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let Some(base) = model_env() else {
        eprintln!("[table_orientation] 缺 pp-lcnet_x1_0_doc_ori.onnx，跳过");
        return;
    };
    let pdf = sample("table_rot90.pdf");
    assert!(pdf.exists(), "缺样本 table_rot90.pdf（gen_table_ori.py 生成）");

    let (co, on, eoff) = run(&pdf, &base, &[]);
    assert_eq!(co, Some(0), "默认档应成功: {eoff}");
    let (cn, off, eon) = run(&pdf, &base, &["ANYDOC_TABLE_ORI"]);
    assert_eq!(cn, Some(0), "显式关闭应成功: {eon}");
    assert_ne!(on, off, "旋转表在默认档必须与关闭态不同，否则 table_ori 没接线");

    // 真值（`tests/gen_table_ori.py`）：4 行数据 × 3 列。
    // OFF 实测 3 行 × 5 列（转置）；ON 实测每行 3 列、行数 ≥4。
    let on_rows = cells_per_row(&on);
    assert!(!on_rows.is_empty(), "ON 应有表格输出:\n{on}");
    assert!(
        on_rows.iter().all(|&n| n == 3),
        "ON 每行应恰 3 格，实际 {on_rows:?}\n{on}"
    );
    assert!(row_count(&on) >= 4, "ON 应 ≥4 行，实际 {}\n{on}", row_count(&on));
    assert!(on.contains("Item"), "ON 应保住表头 Item:\n{on}");
    assert!(on.contains("Quantity"), "ON 应保住表头 Quantity:\n{on}");

    // OFF 侧只需证明"结构不对"——列数不是 3（转置成 5 列）即可。
    let off_rows = cells_per_row(&off);
    assert!(
        off_rows.iter().any(|&n| n != 3),
        "OFF 本应是转置残局（列数≠3）才有对比意义；若 OFF 也全对，说明测试件失效或通路变了:\n{off}"
    );
}
