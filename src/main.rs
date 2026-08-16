//! anydoc-ocr CLI
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anydoc_ocr::ConvertError;
use anydoc_ocr::Result;
use anydoc_ocr::convert_to_markdown;
use anydoc_ocr::models::{OcrLayout, OcrTier};
use anydoc_ocr::quality::QualityRoute;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "anydoc-ocr",
    version,
    about = "办公文档转 Markdown（含图片型 PDF/OFD 的 OCR 回退）"
)]
struct Cli {
    /// 输入文件或目录；目录递归遍历处理所有受支持文档。- 表示 stdin
    input: String,
    /// 输出文件（单文件输入）或输出目录（目录输入）；省略单文件则写 stdout
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// OCR 模型档：tiny/small/medium
    #[arg(long, value_enum, default_value_t = OcrTier::Tiny)]
    ocr_tier: OcrTier,
    /// 版面模型：doc 默认文档结构 / table 表格专用（检出表格才跑 SLANet，无表页零开销）
    #[arg(long, value_enum, default_value_t = OcrLayout::Doc)]
    ocr_layout: OcrLayout,
    /// OFD 强制走 OCR（重建表格结构）
    #[arg(long)]
    ofd_force_ocr: bool,
    /// PDF 强制走 OCR（文字型 PDF 当图片渲染后 OCR，用于图片型校准）
    #[arg(long)]
    pdf_force_ocr: bool,
    /// OCR 推理线程数（页级并行）。A 改造后：进程级 ORT 线程池按
    /// `intra = max(1, 核心数/threads)` 提交，使总线程≈核心数、不再超额订阅。
    /// 默认 0 = 自动取可用并行度（飞腾 D2000 8 核→8），结合 intra=1 全核利用；
    /// 内存受限环境（cgroup<8GB）可显式调小。
    #[arg(long, default_value_t = 0)]
    threads: usize,
    /// 渲染 DPI（图片型 PDF/OFD 走 OCR 时的渲染分辨率）。越低像素越少、渲染与
    /// 文本检测(det)越快，但字号过小会漏检；印刷体公文 100 零精度损失且比 200
    /// 快 33%，80 起脚注/小字开始漏检。实测 上海公报52p: 100 vs 200 恢复率均 99.83%。
    #[arg(long, default_value_t = 100.0)]
    dpi: f32,
    /// ADR-0007：质量路由（后验置信度门控）。auto 用 tiny 跑首页 OCR，平均置信度
    /// 低于阈值则升级 small 全篇重跑（污染件更准）；off 用 --ocr-tier 显式值，
    /// 不承担额外首页 OCR 开销（golden 测试固定 off）。默认 off。
    #[arg(long, value_enum, default_value_t = QualityRoute::Off)]
    quality_route: QualityRoute,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let threads = if cli.threads == 0 {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
    } else {
        cli.threads
    };
    let opts = anydoc_ocr::ConvertOptions {
        ocr_tier: cli.ocr_tier,
        ocr_layout: cli.ocr_layout,
        threads,
        dpi: cli.dpi,
        quality_route: cli.quality_route,
    };
    let force = anydoc_ocr::ForceFlags {
        ofd_force_ocr: cli.ofd_force_ocr,
        pdf_force_ocr: cli.pdf_force_ocr,
    };

    if cli.input == "-" {
        // ADR-0006 审计跟进 W1：单文档路径 `?` 改 `match`，按 e.code() 给精准提示。
        // batch 路径错误隔离继续；单文档路径遇错即终止，故 exit(1)。
        let (path, _tmp) = match resolve_stdin() {
            Ok(v) => v,
            Err(e) => exit_with_hint(&e),
        };
        let md = match convert_to_markdown(&path, &opts, force) {
            Ok(md) => md,
            Err(e) => exit_with_hint(&e),
        };
        write_single(&md, &cli.output)?;
        return Ok(());
    }

    let input = PathBuf::from(&cli.input);
    if input.is_dir() {
        run_batch(&input, &opts, force, &cli.output)?;
    } else {
        let md = match convert_to_markdown(&input, &opts, force) {
            Ok(md) => md,
            Err(e) => exit_with_hint(&e),
        };
        write_single(&md, &cli.output)?;
    }
    Ok(())
}

/// 单文档路径遇错即终止：打印 `失败: {e}\n  提示: {hint}` 后 `exit(1)`。
/// 与 batch 路径共用 `error_hint` 内核（ADR-0006 审计跟进 W1）。
fn exit_with_hint(e: &ConvertError) -> ! {
    eprintln!("转换失败: {e}\n  提示: {}", error_hint(e));
    std::process::exit(1);
}

/// 目录批处理：递归收集文档 → BatchConverter 转换 → 逐文件写出。
fn run_batch(
    input_dir: &PathBuf,
    opts: &anydoc_ocr::ConvertOptions,
    force: anydoc_ocr::ForceFlags,
    output: &Option<PathBuf>,
) -> Result<()> {
    let paths = anydoc_ocr::batch::collect_documents(input_dir);
    if paths.is_empty() {
        eprintln!("[batch] 目录 {} 下无受支持文档", input_dir.display());
        return Ok(());
    }
    let output_dir = output
        .as_ref()
        .ok_or_else(|| anydoc_ocr::ConvertError::Malformed {
            part: None,
            detail: "目录输入需要 --output 指定输出目录".to_string(),
        })?;
    std::fs::create_dir_all(output_dir)?;

    eprintln!(
        "[batch] 发现 {} 个文档，输出到 {}",
        paths.len(),
        output_dir.display()
    );
    let converter = anydoc_ocr::batch::BatchConverter::new(opts.clone(), force);
    let results = converter.convert_many(&paths);
    let mut ok = 0usize;
    let mut fail = 0usize;
    for (i, (path, result)) in paths.iter().zip(results).enumerate() {
        let prefix = format!("[batch] ({}/{})", i + 1, paths.len());
        match result {
            Ok(md) => {
                let out_path = output_dir.join(output_stem(input_dir, path));
                if let Some(parent) = out_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&out_path, md)?;
                eprintln!("{prefix} {} → {}", path.display(), out_path.display());
                ok += 1;
            }
            Err(e) => {
                // ADR-0006 §7：按 e.code() 精准提示（ConvertError 直接有 code() 方法，
                // 无需 downcast）。code() 返稳定字符串，main 据此给"下一步建议"。
                let hint = error_hint(&e);
                eprintln!("{prefix} {} 失败: {e}\n  提示: {hint}", path.display());
                fail += 1;
            }
        }
    }
    eprintln!("[batch] 完成：{ok} 成功，{fail} 失败");
    Ok(())
}

/// 按 `ConvertError::code()` 给用户精准提示（ADR-0006 §7）。
/// code() 返稳定字符串（encrypted/malformed/missingPart/...），main 据此给下一步建议。
fn error_hint(e: &ConvertError) -> &'static str {
    match e.code() {
        "encrypted" => "文档已加密，需提供密码或解密后重试",
        // ADR-0006 审计跟进 S3：`runtime()` 把 ORT/pdfium 失败也归 Malformed（§3 既定，
        // 不能 fork 加变体）。提示文案覆盖两类原因 + 指向 detail，不单押"文档损坏"，
        // 避免对"找不到 libpdfium.so"等环境错误误导归因。
        "malformed" => {
            "文档损坏或运行时错误（如 ORT/pdfium 未配置）— 详见错误详情，检查文件完整性或运行环境"
        }
        "missingPart" => "文档结构不完整（缺必需部件），可能源文件生成不完整",
        "resourceLimit" => "超出安全限制（可能解压炸弹或文档过大）",
        "unsupported" => "格式不支持或需 OCR 但 ORT/pdfium 环境未配置",
        "io" => "文件读写错误（路径不存在/权限不足/磁盘满）",
        _ => "未知错误，详见错误详情",
    }
}

/// 生成输出路径：保持输入目录的相对结构，扩展名换 .md。
/// 例：input_dir=/docs, file=/docs/sub/a.pdf → sub/a.md
fn output_stem(input_dir: &Path, file: &Path) -> PathBuf {
    let rel = file.strip_prefix(input_dir).unwrap_or(file);
    let with_md = rel.with_extension("md");
    if with_md == rel {
        PathBuf::from(format!("{}.md", rel.display()))
    } else {
        with_md
    }
}

fn write_single(md: &str, output: &Option<PathBuf>) -> Result<()> {
    match output {
        Some(o) => std::fs::write(o, md)?,
        None => print!("{md}"),
    }
    Ok(())
}

/// stdin 写入临时文件返回路径（NamedTempFile：随机名 + 用完自动删除）；
/// 返回 Option 持有临时文件句柄，保证转换期间文件存活。
fn resolve_stdin() -> Result<(PathBuf, Option<tempfile::NamedTempFile>)> {
    let mut buf = Vec::new();
    std::io::stdin().read_to_end(&mut buf)?;
    let mut tmp = tempfile::NamedTempFile::new()?;
    Write::write_all(&mut tmp, &buf)?;
    let p = tmp.path().to_path_buf();
    Ok((p, Some(tmp)))
}
