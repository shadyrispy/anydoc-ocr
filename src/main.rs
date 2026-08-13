//! anydoc-ocr CLI
use std::io::{Read, Write};
use std::path::PathBuf;

use anydoc_ocr::{convert_to_markdown, models::OcrLayout, models::OcrTier, ConvertOptions, Error};
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
    /// OCR 推理线程数（页级并行）。注意：oar-ocr 内部已用 ORT 线程池 + 跨页
    /// batching 占满 CPU，外层再加页级并发会过度订阅线程池；小文档 1（或 2）
    /// 最优，多页大文档 4 更快（实测 52p: threads4 比 1 快 ~10%）。
    /// 默认 4 面向大文档；内存受限环境（cgroup<8GB）建议改回 1。
    #[arg(long, default_value_t = 4)]
    threads: usize,
    /// 渲染 DPI（图片型 PDF/OFD 走 OCR 时的渲染分辨率）。越低像素越少、渲染与
    /// 文本检测(det)越快，但字号过小会漏检；印刷体公文 100 零精度损失且比 200
    /// 快 33%，80 起脚注/小字开始漏检。实测 上海公报52p: 100 vs 200 恢复率均 99.83%。
    #[arg(long, default_value_t = 100.0)]
    dpi: f32,
}

fn main() -> Result<(), Error> {
    let cli = Cli::parse();
    let (path, _tmp) = resolve_input(&cli.input)?;
    let opts = ConvertOptions {
        ocr_tier: cli.ocr_tier,
        ocr_layout: cli.ocr_layout,
        ofd_force_ocr: cli.ofd_force_ocr,
        pdf_force_ocr: cli.pdf_force_ocr,
        threads: cli.threads,
        dpi: cli.dpi,
    };
    match convert_to_markdown(&path, &opts) {
        Ok(md) => {
            match cli.output {
                Some(o) => std::fs::write(&o, md)?,
                None => print!("{md}"),
            }
            Ok(())
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(e.exit_code());
        }
    }
}

/// stdin 写入临时文件返回路径（NamedTempFile：随机名 + 用完自动删除）；
/// 返回 Option 持有临时文件句柄，保证转换期间文件存活。
fn resolve_input(
    input: &str,
) -> anyhow::Result<(PathBuf, Option<tempfile::NamedTempFile>)> {
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
