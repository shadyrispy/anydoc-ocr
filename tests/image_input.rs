//! #12 位图输入 + #13 `--text-only` 的入口回归。
//!
//! 子进程走真实 CLI（同 `limits_gates.rs`：edition-2024 下 `set_var` 不安全，且
//! 这些判定要在**进程启动前**定格）。
//!
//! 分两层，刻意让**闸与分流层零模型依赖**——CI 无模型时该绿的一定要绿：
//! 1. 不碰模型（本文件全部默认跑）：图片魔数/扩展名分流、像素闸在**解码前**开火
//!    （超限不可能已经建引擎）、`--text-only` 与 force 系互斥、纯扫描件/图片在
//!    `--text-only` 下显式 `needsOcr`（而不是静默出半篇）、`--pages` 对图片拒绝；
//! 2. 要模型（缺 `OAR_HOME`/`ANYDOC_MODEL_DIR` 即跳过）：真图 → Markdown 端到端。
//!
//! 口径：超限**显式报错、不静默降采样**（与 PDF 渲染路径的自动降 scale 是两件事，
//! 见 README「已知限制」安全闸一条）。

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
    Run { code: child.wait().expect("wait").code(), out, err }
}

/// 测试独立目录（并行互不干扰）。
fn tmpdir(test: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("anydoc_img_{test}_{}", std::process::id()));
    std::fs::create_dir_all(&d).expect("mkdir");
    d
}

/// 造一张纯色 PNG（`image` 编码，无需任何样本资产）。
/// 显式走 `PngEncoder`：`DynamicImage::save` 按扩展名推格式，无扩展名目标（本文件
/// 就要造这种形状）会直接报 `Format(Unknown)`。
fn write_png(dir: &Path, name: &str, w: u32, h: u32) -> PathBuf {
    use image::codecs::png::PngEncoder;
    use image::{ImageEncoder, RgbaImage};
    let p = dir.join(name);
    let f = std::fs::File::create(&p).expect("create png");
    PngEncoder::new(f)
        .write_image(
            RgbaImage::from_pixel(w, h, image::Rgba([255, 255, 255, 255])).as_raw(),
            w,
            h,
            image::ExtendedColorType::Rgba8,
        )
        .expect("encode png");
    p
}

/// #12：合法小图 + `--text-only` → `needsOcr`，**不静默出空文档**。
/// 选 `--text-only` 当探针的原因：它在图片分支上立即返回，因此这条断言不需要
/// 任何模型（图片通道本身是纯 OCR，没有文字层可退）。
#[test]
fn image_under_text_only_is_needs_ocr() {
    let dir = tmpdir("txtonly");
    let p = write_png(&dir, "small.png", 32, 32);
    let r = run(&[p.to_str().unwrap(), "--text-only", "-o", "/dev/null"], &[]);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(r.code, Some(1), "图片 + --text-only 应失败: stdout={}\n{}", r.out, r.err);
    assert!(r.err.contains("needs OCR") && r.err.contains("图片"), "应点名 needsOcr 与图片: {}", r.err);
    assert!(r.out.is_empty(), "失败路径不得写出内容");
}

/// #12：长边超 `ANYDOC_RENDER_EDGE_CAP` → `resourceLimit`，且发生在**解码/建引擎之前**。
/// 用 `ANYDOC_MAX_INPUT_BYTES` 把图也压不过大小闸之外？不——这里显式调小 cap 造超限，
/// 断言里同时要求**不得**先撞大小闸（两闸错误串了就是回归）。
#[test]
fn oversize_image_rejected_before_decode() {
    let dir = tmpdir("big");
    // cap=64 → 128px 长边即超限；PNG 编码后仅数百字节，大小闸（默认 200MiB）绝不触发
    let p = write_png(&dir, "wide.png", 128, 40);
    let r = run(
        &[p.to_str().unwrap(), "-o", "/dev/null"],
        &[("ANYDOC_RENDER_EDGE_CAP", "64")],
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(r.code, Some(1), "超限图应失败: {}", r.err);
    assert!(
        r.err.contains("resource limit exceeded") && r.err.contains("长边"),
        "应点名像素闸: {}",
        r.err
    );
    assert!(!r.err.contains("ANYDOC_MAX_INPUT_BYTES"), "不得撞错闸: {}", r.err);
    assert!(!r.err.contains("[ocr]"), "闸必须在 OCR 之前: {}", r.err);
}

/// #12 回归护栏：**无扩展名**（stdin 落地的 NamedTempFile 形态）也要按魔数认出图片，
/// 否则会被当 `Other` 丢给 anydoc 兜底、报"格式不支持"这种误导结论。
#[test]
fn extensionless_png_is_detected_as_image() {
    let dir = tmpdir("noext");
    let p = write_png(&dir, "blob", 32, 32);
    let r = run(&[p.to_str().unwrap(), "--text-only", "-o", "/dev/null"], &[]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        r.err.contains("needs OCR") && r.err.contains("图片"),
        "无扩展名 PNG 应经魔数进图片通道（而非 anydoc 兜底）: {}",
        r.err
    );
}

/// #12：`--pages` 仅 PDF（图片无页概念）→ `unsupported`，语法合法也不例外。
#[test]
fn pages_flag_rejected_for_image() {
    let dir = tmpdir("pages");
    let p = write_png(&dir, "a.png", 32, 32);
    let r = run(&[p.to_str().unwrap(), "--pages", "1", "-o", "/dev/null"], &[]);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(r.code, Some(1), "--pages 用于图片应失败: {}", r.err);
    assert!(r.err.contains("unsupported"), "应报 unsupported: {}", r.err);
}

/// #12：`jp2` 有 MinerU 的扩展名与魔数，但 `image` 0.25 无 JPEG2000 解码器——
/// 必须**显式 unsupported**，而不是静默掉进 anydoc 兜底或"成功但空"。
#[test]
fn jp2_is_honestly_unsupported() {
    let dir = tmpdir("jp2");
    // JP2 签名盒：长度 12 + `jP  `（两个空格）+ CR LF 0x87 LF
    let mut bytes: Vec<u8> = vec![0, 0, 0, 12, b'j', b'P', b' ', b' ', b'\r', b'\n', 0x87, b'\n'];
    bytes.resize(4096, 0); // 假 jp2 头（本仓不解码，只求分流命中）
    let p = dir.join("a.jp2");
    std::fs::write(&p, &bytes).expect("write");
    let r = run(&[p.to_str().unwrap(), "-o", "/dev/null"], &[]);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(r.code, Some(1), "jp2 应失败: {}", r.err);
    assert!(
        r.err.contains("unsupported") && !r.err.contains("needs OCR"),
        "jp2 应报 unsupported（解码器缺失），不能被兜底通道或 needsOcr 顶替: {}",
        r.err
    );
}

/// #13：`--text-only` 与两个 force 开关互斥，且是**纯参数校验**——输入文件不存在
/// 也必须先报互斥（否则错误随文档内容漂移，且白付一次 IO）。
#[test]
fn text_only_conflicts_with_force_flags() {
    for force in ["--pdf-force-ocr", "--ofd-force-ocr"] {
        let r = run(&["/nonexistent/anydoc_txtonly.pdf", "--text-only", force, "-o", "/dev/null"], &[]);
        assert_eq!(r.code, Some(1), "--text-only + {force} 应失败: {}", r.err);
        assert!(
            r.err.contains("unsupported") && r.err.contains("互斥"),
            "{force} 应与 --text-only 互斥并点名: {}",
            r.err
        );
        assert!(!r.err.contains("No such file"), "参数校验应先于 IO: {}", r.err);
    }
}

/// #13：图片型 PDF（无文字层）+ `--text-only` → `needsOcr`，绝不静默产出空 Markdown。
#[test]
fn scanned_pdf_under_text_only_is_needs_ocr() {
    let pdf = sample("image.pdf");
    if !pdf.exists() {
        eprintln!("[image_input] skip: samples/image.pdf 缺失");
        return;
    }
    let r = run(&[pdf.to_str().unwrap(), "--text-only", "-o", "/dev/null"], &[]);
    assert_eq!(r.code, Some(1), "扫描件 + --text-only 应失败: {}", r.err);
    assert!(
        r.err.contains("needs OCR") && r.err.contains("--text-only"),
        "应点名 needsOcr 与开关本身: {}",
        r.err
    );
}

/// #13：混合文档（部分页有文字层）+ `--text-only` → **成功**出文字层内容，并把缺页
/// 号打到 stderr。守的是两件事：不静默丢页（必须点名页号），也不越权跑 OCR。
#[test]
fn hybrid_pdf_under_text_only_warns_with_page_numbers() {
    let pdf = sample("mixed_scan.pdf");
    if !pdf.exists() {
        eprintln!("[image_input] skip: samples/mixed_scan.pdf 缺失");
        return;
    }
    let r = run(&[pdf.to_str().unwrap(), "--text-only", "-o", "/dev/null"], &[]);
    assert_eq!(r.code, Some(0), "混合文档按文字层输出应成功: {}", r.err);
    assert!(
        r.err.contains("--text-only 不跑 OCR"),
        "必须显式告警（不静默）: {}",
        r.err
    );
    assert!(
        r.err.contains("页无文字层内容") && r.err.chars().any(|c| c.is_ascii_digit()),
        "告警应列出缺页号: {}",
        r.err
    );
    assert!(!r.err.contains("[ocr]"), "告警之后不得再进 OCR: {}", r.err);
}

/// #13 的验收本体：**没有任何模型可加载**时，文字型 PDF/OFD 仍要成功出内容。
/// 做法 = 把 `OAR_HOME` 指到空目录、`ANYDOC_MODEL_DIR` 清掉（子进程 env 显式覆盖）。
/// 若探针里还藏着一次模型加载，这条会在下载失败处红掉——正是该守住的契约。
/// PDF 与 OFD 是**两处独立实现**（`confirm_table_pages` 早退 vs `convert_ofd` 跳过
/// `init_runtime`），故两个样本都要过。
#[test]
fn text_only_works_with_an_empty_model_home() {
    for rel in ["text.pdf", "text.ofd", "text_font.ofd"] {
        let pdf = sample(rel);
        if !pdf.exists() {
            eprintln!("[image_input] skip: samples/{rel} 缺失");
            continue;
        }
        let empty = tmpdir("emptyhome");
        let out = empty.join("o.md");
        let r = run(
            &[pdf.to_str().unwrap(), "--text-only", "-o", out.to_str().unwrap()],
            &[
                ("OAR_HOME", empty.to_str().unwrap()),
                ("ANYDOC_MODEL_DIR", ""),
                ("HOME", empty.to_str().unwrap()),
            ],
        );
        let md = std::fs::read_to_string(&out).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&empty);
        assert_eq!(r.code, Some(0), "{rel}: 空模型目录下文字层直出应成功: {}", r.err);
        assert!(!md.trim().is_empty(), "{rel}: 应有文字层内容");
        assert!(
            !r.err.to_lowercase().contains("download") && !r.err.contains("下载"),
            "{rel}: --text-only 不该触发任何模型下载: {}",
            r.err
        );
    }
}

/// #13 OFD 侧：图片型 OFD（每页都要 OCR）在 `--text-only` 下显式 `needsOcr`，
/// 而不是静默出一篇空壳。
#[test]
fn image_ofd_under_text_only_is_needs_ocr() {
    let ofd = sample("image.ofd");
    if !ofd.exists() {
        eprintln!("[image_input] skip: samples/image.ofd 缺失");
        return;
    }
    let r = run(&[ofd.to_str().unwrap(), "--text-only", "-o", "/dev/null"], &[]);
    assert_eq!(r.code, Some(1), "图片型 OFD + --text-only 应失败: {}", r.err);
    assert!(
        r.err.contains("needs OCR") && r.err.contains("--text-only"),
        "应点名 needsOcr 与开关本身: {}",
        r.err
    );
}

/// #13 OFD 侧：与 `--ofd-force-ocr` 的互斥在**库侧通道**也有同名闸（CLI 早拒之
/// 外），这里同时验证 OFD 通道不依赖 PDF 那条判定。
#[test]
fn text_only_conflicts_with_ofd_force_at_library_gate() {
    let ofd = sample("text.ofd");
    if !ofd.exists() {
        eprintln!("[image_input] skip: samples/text.ofd 缺失");
        return;
    }
    // 走 CLI：两开关同给，早拒发生在参数层（文件存在与否都一样）
    let r = run(&[ofd.to_str().unwrap(), "--text-only", "--ofd-force-ocr", "-o", "/dev/null"], &[]);
    assert_eq!(r.code, Some(1), "应失败: {}", r.err);
    assert!(r.err.contains("unsupported") && r.err.contains("互斥"), "应报互斥: {}", r.err);
}

/// 端到端（要模型，缺则跳过）：真图能进 OCR 通路。
/// 走默认 `mineru-basic` 档，与图片型 PDF 同一条通路。门控条件 = `$OAR_HOME` 下
/// 有 MinerU 必需件（只有 `pp-doclayoutv2.onnx` 一件即够判"这套资产已就位"），
/// 否则该测试会去联网拉 214MB——CI/离线机上不该发生。
#[test]
fn png_ocr_end_to_end() {
    let Some(home) = std::env::var("OAR_HOME").ok().filter(|v| !v.is_empty()) else {
        eprintln!("[image_input] skip png_ocr_end_to_end: 缺 OAR_HOME");
        return;
    };
    if !Path::new(&home).join("pp-doclayoutv2.onnx").exists() {
        eprintln!("[image_input] skip png_ocr_end_to_end: {home} 下无 MinerU 版面模型");
        return;
    }
    let dir = tmpdir("e2e");
    // 纯色小图不校精度（精度归 golden），只守"分流 → 引擎 → 出文"这条通路本身。
    let p = write_png(&dir, "t.png", 64, 48);
    let r = run(&[p.to_str().unwrap(), "-o", "/dev/null"], &[("OAR_HOME", &home)]);
    let _ = std::fs::remove_dir_all(&dir);
    if r.code == Some(0) {
        return; // 通路打通
    }
    assert!(
        r.err.contains("runtime") || r.err.contains("model") || r.err.contains("download"),
        "失败只允许是模型/运行时问题，不能是分流或闸: {}",
        r.err
    );
}
