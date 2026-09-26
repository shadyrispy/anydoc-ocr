//! 审计 #8/#9 入口闸回归（子进程走真实 CLI，避免 edition-2024 `set_var` 竞争）：
//!
//! - #8 大小闸：stdin 有界读（超限即断读，不再无界吃内存）、文件入口（含 HTML
//!   这类 anydoc 通道文档）超限显式 `resourceLimit`，**且报错发生在 OCR 之前**；
//!   未超限照常处理（闸不漏放）；`ANYDOC_MAX_INPUT_BYTES` 覆盖生效。
//! - #9 dpi 闸：`--dpi` 越界/NaN 立即 `unsupported`，不跑半途。
//!
//! 口径对齐 MinerU 413（`upload_size_mismatch`）：拒绝并说明，绝不静默截断。

use std::io::Read;
use std::io::Write;
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

/// 伪 PDF 载荷：`%PDF` 魔数（过 detect）+ 垃圾（本就无法解析，只求在闸处停下）。
fn fake_pdf(len: usize) -> Vec<u8> {
    let mut v = b"%PDF-1.7\n".to_vec();
    v.resize(len, b'x');
    v
}

struct Run {
    code: Option<i32>,
    out: String,
    err: String,
}

/// 跑 CLI：`stdin` 为 `Some` 时输入取 `-`（管道投喂）。
fn run(args: &[&str], stdin: Option<&[u8]>, envs: &[(&str, &str)]) -> Run {
    let mut cmd = Command::new(bin());
    cmd.args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    if stdin.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn anydoc-ocr");
    if let Some(bytes) = stdin {
        let mut si = child.stdin.take().expect("stdin pipe");
        // 超限场景子进程会在闸处提前退出 → 管道写端 BrokenPipe 属预期，
        // 不作为测试失败；断言只看退出码与 stderr。
        let _ = si.write_all(bytes);
        let _ = si.flush();
        drop(si); // 关闭写端，子进程读到 EOF
    }
    let mut out = String::new();
    let mut err = String::new();
    child
        .stdout
        .take()
        .expect("stdout")
        .read_to_string(&mut out)
        .expect("read stdout");
    child
        .stderr
        .take()
        .expect("stderr")
        .read_to_string(&mut err)
        .expect("read stderr");
    let status = child.wait().expect("wait");
    Run {
        code: status.code(),
        out,
        err,
    }
}

/// #8 stdin：超限 → `resourceLimit`，且不触碰 OCR（错误发生在闸，不在管线深处）。
#[test]
fn stdin_oversize_rejected_as_resource_limit() {
    let r = run(
        &["-", "-o", "/dev/null"],
        Some(&fake_pdf(1024 * 1024)),
        &[("ANYDOC_MAX_INPUT_BYTES", "65536")],
    );
    assert_eq!(r.code, Some(1), "应失败: stdout={}\nstderr={}", r.out, r.err);
    assert!(
        r.err.contains("resource limit exceeded") && r.err.contains("ANYDOC_MAX_INPUT_BYTES"),
        "stderr 应点名 resourceLimit 与可调变量: {}",
        r.err
    );
    assert!(!r.err.contains("[ocr]"), "闸应在 OCR 之前: {}", r.err);
}

/// #8 stdin：未超限 → 闸不误伤（此处由后续解析报 malformed，而非大小错）。
#[test]
fn stdin_under_limit_not_blocked_by_size_gate() {
    let r = run(
        &["-", "-o", "/dev/null"],
        Some(&fake_pdf(32 * 1024)),
        &[("ANYDOC_MAX_INPUT_BYTES", "65536")],
    );
    assert_eq!(r.code, Some(1), "垃圾 PDF 应失败: {}", r.err);
    assert!(
        !r.err.contains("resource limit exceeded"),
        "未超限不得报大小闸: {}",
        r.err
    );
}

/// #8 文件入口：超限 → `resourceLimit`（detail 指向真实路径）。
#[test]
fn file_oversize_rejected_as_resource_limit() {
    let dir = std::env::temp_dir().join(format!("anydoc_limits_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let p = dir.join("big.pdf");
    std::fs::write(&p, fake_pdf(1024 * 1024)).expect("write");
    let r = run(
        &[p.to_str().unwrap(), "-o", "/dev/null"],
        None,
        &[("ANYDOC_MAX_INPUT_BYTES", "65536")],
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(r.code, Some(1), "应失败: {}", r.err);
    assert!(
        r.err.contains("resource limit exceeded") && r.err.contains("big.pdf"),
        "stderr 应含 resourceLimit 与文件名: {}",
        r.err
    );
}

/// #8 非 PDF 通道同样有闸：HTML 经 `route_doc` 统一入口，超限即拒
/// （审计原语：HTML/CSV 此前无任何字节上限，超大文件全量读进解析器）。
#[test]
fn html_oversize_rejected_as_resource_limit() {
    let dir = std::env::temp_dir().join(format!("anydoc_limits_html_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let p = dir.join("big.html");
    let mut html = b"<html><body>".to_vec();
    html.resize(1024 * 1024, b'a');
    std::fs::write(&p, html).expect("write");
    let r = run(
        &[p.to_str().unwrap(), "-o", "/dev/null"],
        None,
        &[("ANYDOC_MAX_INPUT_BYTES", "65536")],
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(r.code, Some(1), "应失败: {}", r.err);
    assert!(
        r.err.contains("resource limit exceeded"),
        "HTML 入口应经同一大小闸: {}",
        r.err
    );
}

/// #9 dpi 闸：低于下限（`--dpi 10`）→ `unsupported`，点名允许区间。
#[test]
fn dpi_below_range_rejected() {
    let pdf = sample("multipage.pdf");
    if !pdf.exists() {
        eprintln!("[limits] skip dpi_below_range_rejected: multipage.pdf 缺失");
        return;
    }
    let r = run(&[pdf.to_str().unwrap(), "--dpi", "10", "-o", "/dev/null"], None, &[]);
    assert_eq!(r.code, Some(1), "非法 dpi 应失败: {}", r.err);
    assert!(
        r.err.contains("unsupported") && r.err.contains("允许区间"),
        "stderr 应点名 dpi 区间: {}",
        r.err
    );
}

/// #9 dpi 闸：`NaN`/`inf` 能被 clap 解析成 f32，必须被库侧校验拦下（否则
/// `scale=NaN` 一路乘进渲染尺寸）。
#[test]
fn dpi_non_finite_rejected() {
    let pdf = sample("multipage.pdf");
    if !pdf.exists() {
        eprintln!("[limits] skip dpi_non_finite_rejected: multipage.pdf 缺失");
        return;
    }
    for bad in ["nan", "inf"] {
        let r = run(&[pdf.to_str().unwrap(), "--dpi", bad, "-o", "/dev/null"], None, &[]);
        assert_eq!(r.code, Some(1), "dpi={bad} 应失败: {}", r.err);
        assert!(r.err.contains("unsupported"), "dpi={bad} 应报 unsupported: {}", r.err);
    }
}

/// 默认闸（200 MiB）不影响正常样本文档：multipage.pdf 经 stdin 正常出 Markdown。
#[test]
fn default_limit_lets_normal_docs_through() {
    let pdf = sample("multipage.pdf");
    if !pdf.exists() {
        eprintln!("[limits] skip default_limit_lets_normal_docs_through: multipage.pdf 缺失");
        return;
    }
    let bytes = std::fs::read(&pdf).expect("read sample");
    let out = std::env::temp_dir().join(format!("anydoc_limits_ok_{}.md", std::process::id()));
    let r = run(&["-", "-o", out.to_str().unwrap()], Some(&bytes), &[]);
    let md = std::fs::read_to_string(&out).unwrap_or_default();
    let _ = std::fs::remove_file(&out);
    assert_eq!(r.code, Some(0), "默认配置应成功: {}", r.err);
    assert!(!md.trim().is_empty(), "输出不应为空");
}
