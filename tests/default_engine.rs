//! "命令行默认使用 mineru 的流程和模型"这条需求本身的回归网（子进程走真实 CLI）。
//!
//! 三组断言，由弱到强：
//! 1. **参数面**：`--help` 里默认档写着 `mineru-basic`；被撤下的
//!    `--ocr-layout` / `--quality-route` 必须**不再被接受**（撤参数是本次需求的
//!    一半——只留着不用，用户仍会走进与默认流程互斥的路径）；
//! 2. **默认 == 显式 mineru-basic**：同一图片型 PDF，不传参数与显式传
//!    `--ocr-tier mineru-basic` 输出**逐字节一致**（这才叫"默认走该档"）；
//! 3. **默认 != 旧默认 tiny**：默认档输出与 `--ocr-tier tiny` 必须可测地不同，
//!    否则第 2 条可能被"两边都退化成 tiny"假阳性满足。
//!
//! 第 1 组不需要模型；第 2/3 组需要 MinerU 必需件在**本测试所用的缓存根**
//! （`$OAR_HOME`，未设则 `~/.oar`，与上游 `download::cache_dir` 同规则）有**已校验**
//! 缓存（缺则自跳过——CI 无模型时不该红，与 `tests/ocr_post.rs` 同口径）。
//! 刻意**不依赖** `ANYDOC_MODEL_DIR`：证明普通安装（只有 OAR_HOME 自动下载缓存）
//! 也能跑完默认档，公式件缺失只是不输出 LaTeX。
//!
//! 运行前提：
//! ```text
//! export LD_LIBRARY_PATH=<ort>/lib:<pdfium>/lib
//! # 可选：export OAR_HOME=~/.oar（需含 pp-doclayoutv2.onnx 等已校验缓存）
//! ```

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// OCR 懒初始化并发加载模型有 SIGTRAP 竞态（同 hybrid.rs / ocr_post.rs）。
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

fn run(args: &[&str]) -> Run {
    let mut cmd = Command::new(bin());
    cmd.args(args);
    // 自备模型目录会改变默认档的加载路径（公式件），令 2/3 组断言依赖环境；
    // 显式清除，测试只认 $OAR_HOME 缓存这一条普通安装路径。
    cmd.env_remove("ANYDOC_MODEL_DIR");
    // 同口径钉死缓存根：不设 OAR_HOME 时上游回落 `~/.oar`，而 CI 上 HOME 未指向
    // 预热缓存目录（release.yml 全程显式 export OAR_HOME=<workspace>/.oar-home），
    // mineru_cached() 会误判缺件、第 2/3 组断言静默自跳过——那就是"绿但什么都没测"。
    // 故缓存根为空时直接把本次运行指到默认的 ~/.oar，判定与取数用同一个目录。
    if std::env::var_os("OAR_HOME").filter(|v| !v.is_empty()).is_none() {
        if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
            cmd.env("OAR_HOME", Path::new(&home).join(".oar"));
        }
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn anydoc-ocr");
    let mut out = String::new();
    let mut err = String::new();
    child.stdout.take().expect("stdout").read_to_string(&mut out).expect("read stdout");
    child.stderr.take().expect("stderr").read_to_string(&mut err).expect("read stderr");
    Run { code: child.wait().expect("wait").code(), out, err }
}

/// MinerU 必需件是否已在**本测试实际使用的缓存根**（`$OAR_HOME`，空则 `~/.oar`）
/// 校验缓存齐全（缺 → `false`，调用方跳过）。
///
/// 判据必须与 [`run`] 注入给子进程的缓存根一致，否则会出现"测试判缺件而自跳过、
/// 子进程其实有缓存"的假绿。清单在此**硬编码**而非引用 `models::MINERU_ASSETS`：
/// 集成测试读被测常量就抓不到"预检清单与实际加载需求脱节"这一类 bug
/// （单测已覆盖一致性，这里覆盖事实）。
fn mineru_cached() -> bool {
    let dir = match std::env::var_os("OAR_HOME").filter(|v| !v.is_empty()) {
        Some(home) => PathBuf::from(home),
        None => match std::env::var_os("HOME").filter(|v| !v.is_empty()) {
            Some(h) => Path::new(&h).join(".oar"),
            None => return false,
        },
    };
    [
        "pp-doclayoutv2.onnx",
        "pp-ocrv6_tiny_det.onnx",
        "pp-ocrv6_small_rec.onnx",
        "ppocrv6_dict.txt",
        "slanet_plus.onnx",
        "pp-lcnet_x1_0_table_cls.onnx",
        "table_structure_dict_ch.txt",
    ]
    .iter()
    // .sha256 伴生文件只在 hash 校验通过后写入，故"文件在 + 伴生在"= 已校验缓存
    .all(|n| dir.join(n).is_file() && dir.join(format!(".{n}.sha256")).is_file())
}

macro_rules! need_model {
    ($tag:literal) => {
        if !mineru_cached() {
            eprintln!("[default_engine] $OAR_HOME 无 MinerU 已校验缓存，跳过");
            return;
        }
    };
}

// ── 1. 参数面（无需模型） ──

#[test]
fn help_declares_mineru_as_the_default_tier() {
    let r = run(&["--help"]);
    assert_eq!(r.code, Some(0), "--help 应成功: {}", r.err);
    let help = format!("{}{}", r.out, r.err);
    assert!(help.contains("mineru-basic"), "--help 未提到默认档 mineru-basic:\n{help}");
    assert!(help.contains("MinerU"), "--help 应说明默认档与 MinerU 的对应关系:\n{help}");
}

#[test]
fn dropped_flags_are_rejected() {
    let pdf = sample("text.pdf");
    assert!(pdf.exists(), "缺样本 text.pdf");
    for flag in ["--ocr-layout", "--quality-route"] {
        let r = run(&[pdf.to_str().unwrap(), flag, "off", "-o", "/dev/null"]);
        assert_eq!(
            r.code,
            Some(2),
            "{flag} 应被 clap 拒绝（退出 2），实际 {:?}——撤参数未生效？\n{}",
            r.code,
            r.err
        );
        assert!(
            r.err.contains("unexpected argument"),
            "{flag} 的报错应点名未知参数，实际: {}",
            r.err
        );
    }
}

/// 反向守护：撤 `--ocr-layout` 时别把 `--ocr-tier` 一起写坏。
#[test]
fn kept_flags_still_work() {
    let pdf = sample("text.pdf");
    let r = run(&[pdf.to_str().unwrap(), "--ocr-tier", "tiny", "-o", "/dev/null"]);
    assert_eq!(r.code, Some(0), "--ocr-tier tiny 仍应可用: {}", r.err);
}

// ── 2. 默认 == 显式 mineru-basic ──

#[test]
fn default_invocation_equals_explicit_mineru_basic() {
    need_model!("默认档等值");
    let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let pdf = sample("image_table.pdf");
    assert!(pdf.exists(), "缺样本 image_table.pdf");
    let a = run(&[pdf.to_str().unwrap(), "--dpi", "150"]);
    let b = run(&[pdf.to_str().unwrap(), "--dpi", "150", "--ocr-tier", "mineru-basic"]);
    assert_eq!(a.code, Some(0), "默认档应成功: {}", a.err);
    assert_eq!(b.code, Some(0), "显式 mineru-basic 应成功: {}", b.err);
    assert!(!a.out.trim().is_empty(), "默认档输出不应为空");
    assert_eq!(a.out, b.out, "默认档 ≠ mineru-basic（默认值没生效？）");
}

// ── 3. 默认 != 旧默认 tiny ──

/// `multipage.pdf` 含需 OCR 的页，版面模型不同 → 块切分与行内空格可测地不同
/// （已实测：默认档与 tiny 输出 DIFF；`image.pdf` 这类单行标题样本则 SAME，
/// 故选用多页正文样本做这条判别断言）。
#[test]
fn default_differs_from_the_old_tiny_default() {
    need_model!("默认档区别于 tiny");
    let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let pdf = sample("multipage.pdf");
    assert!(pdf.exists(), "缺样本 multipage.pdf");
    let d = run(&[pdf.to_str().unwrap()]);
    let t = run(&[pdf.to_str().unwrap(), "--ocr-tier", "tiny"]);
    assert_eq!(d.code, Some(0), "默认档应成功: {}", d.err);
    assert_eq!(t.code, Some(0), "tiny 档应成功: {}", t.err);
    assert_ne!(
        d.out, t.out,
        "默认档与 tiny 输出完全相同 → 默认值可能仍是 tiny（golden 基线会假绿）"
    );
}
