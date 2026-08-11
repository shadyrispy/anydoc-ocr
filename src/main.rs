//! anydoc-ocr CLI
use std::io::{Read, Write};
use std::path::PathBuf;

use anydoc_ocr::{ConvertOptions, convert_to_markdown, models::OcrLayout, models::OcrTier};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "anydoc-ocr",
    version,
    about = "办公文档转 Markdown（含图片型 PDF/OFD 的 OCR 回退）"
)]
struct Cli {
    /// 输入文件；- 表示 stdin（图片型 PDF/OFD 先落临时文件）
    input: String,
    /// 输出文件；省略则写 stdout
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
    let (path, _tmp) = resolve_input(&cli.input)?;
    // threads==0 → 自动取可用并行度（A：配合 intra=核心数/threads 全核利用）。
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
    let md = convert_to_markdown(&path, &opts)?;
    match cli.output {
        Some(o) => std::fs::write(&o, md)?,
        None => print!("{md}"),
    }
    Ok(())
}

/// stdin 写入临时文件返回路径（NamedTempFile：随机名 + 用完自动删除）；
/// 返回 Option 持有临时文件句柄，保证转换期间文件存活。
fn resolve_input(input: &str) -> anyhow::Result<(PathBuf, Option<tempfile::NamedTempFile>)> {
    if input == "-" {
        let mut buf = Vec::new();
        std::io::stdin().read_to_end(&mut buf)?;
        let mut tmp = tempfile::NamedTempFile::new()?;
        Write::write_all(&mut tmp, &buf)?;
        let p = tmp.path().to_path_buf();
        Ok((p, Some(tmp)))
    } else {
        Ok((PathBuf::from(input), None))
    }
}
