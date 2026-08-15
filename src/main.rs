//! anydoc-ocr CLI
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anydoc_ocr::{ConvertOptions, convert_to_markdown, models::OcrLayout, models::OcrTier};
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
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let threads = if cli.threads == 0 {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
    } else {
        cli.threads
    };
    let opts = ConvertOptions {
        ocr_tier: cli.ocr_tier,
        ocr_layout: cli.ocr_layout,
        ofd_force_ocr: cli.ofd_force_ocr,
        pdf_force_ocr: cli.pdf_force_ocr,
        threads,
        dpi: cli.dpi,
    };

    if cli.input == "-" {
        let (path, _tmp) = resolve_stdin()?;
        let md = convert_to_markdown(&path, &opts)?;
        write_single(&md, &cli.output)?;
        return Ok(());
    }

    let input = PathBuf::from(&cli.input);
    if input.is_dir() {
        run_batch(&input, &opts, &cli.output)?;
    } else {
        let md = convert_to_markdown(&input, &opts)?;
        write_single(&md, &cli.output)?;
    }
    Ok(())
}

/// 目录批处理：递归收集文档 → BatchConverter 转换 → 逐文件写出。
fn run_batch(
    input_dir: &PathBuf,
    opts: &ConvertOptions,
    output: &Option<PathBuf>,
) -> anyhow::Result<()> {
    let paths = anydoc_ocr::batch::collect_documents(input_dir);
    if paths.is_empty() {
        eprintln!("[batch] 目录 {} 下无受支持文档", input_dir.display());
        return Ok(());
    }
    let output_dir = output.as_ref().ok_or_else(|| {
        anyhow::anyhow!("目录输入需要 --output 指定输出目录")
    })?;
    std::fs::create_dir_all(output_dir)?;

    eprintln!("[batch] 发现 {} 个文档，输出到 {}", paths.len(), output_dir.display());
    let converter = anydoc_ocr::batch::BatchConverter::new(opts.clone());
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
                eprintln!("{prefix} {} 失败: {e}", path.display());
                fail += 1;
            }
        }
    }
    eprintln!("[batch] 完成：{ok} 成功，{fail} 失败");
    Ok(())
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

fn write_single(md: &str, output: &Option<PathBuf>) -> anyhow::Result<()> {
    match output {
        Some(o) => std::fs::write(o, md)?,
        None => print!("{md}"),
    }
    Ok(())
}

/// stdin 写入临时文件返回路径（NamedTempFile：随机名 + 用完自动删除）；
/// 返回 Option 持有临时文件句柄，保证转换期间文件存活。
fn resolve_stdin() -> anyhow::Result<(PathBuf, Option<tempfile::NamedTempFile>)> {
    let mut buf = Vec::new();
    std::io::stdin().read_to_end(&mut buf)?;
    let mut tmp = tempfile::NamedTempFile::new()?;
    Write::write_all(&mut tmp, &buf)?;
    let p = tmp.path().to_path_buf();
    Ok((p, Some(tmp)))
}
