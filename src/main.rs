//! anydoc-ocr CLI
use std::io::Write;
use std::path::{Path, PathBuf};

use anydoc_ocr::ConvertError;
use anydoc_ocr::Result;
use anydoc_ocr::convert_to_markdown;
use anydoc_ocr::models::{MINERU_ENGINE_HELP, OcrLayout, OcrTier};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "anydoc-ocr",
    version,
    about = "办公文档转 Markdown（含图片型 PDF/OFD 的 OCR 回退）",
    after_help = MINERU_ENGINE_HELP
)]
struct Cli {
    /// 输入文件或目录；目录递归遍历处理所有受支持文档。- 表示 stdin
    /// （图片输入见下方"输入格式"说明）
    input: String,
    /// 输出文件（单文件输入）或输出目录（目录输入）；省略单文件则写 stdout
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// OCR 引擎档（默认 mineru-basic = MinerU 4.0 basic 同流程同模型，无需显式传）。
    /// 只在默认档于本机跑不动时才需要碰：内存受限、无 MinerU 资产、离线安装包内
    /// 置的小模型 → 降 tiny/small/medium。详见 --help 末尾档位说明。
    #[arg(long, value_enum, default_value_t = OcrTier::MineruBasic)]
    ocr_tier: OcrTier,
    /// OFD 强制走 OCR（重建表格结构）
    #[arg(long)]
    ofd_force_ocr: bool,
    /// PDF 强制走 OCR（文字型 PDF 当图片渲染后 OCR，用于图片型校准）
    #[arg(long)]
    pdf_force_ocr: bool,
    /// 只走文字层，**绝不跑 OCR、绝不加载模型**（#13）。用途：这台机器不联网/
    /// 没模型也要出文字层内容，以及排查"是不是 OCR 的锅"。
    /// 与 MinerU `--ocr-mode txt` 的差别：MinerU 在 medium 档仍会为图片块加载
    /// 版面/OCR，本开关严格——全篇无文字层的扫描件直接报 needsOcr，混合文档
    /// 按文字层输出并把缺页号打到 stderr（不静默）。图片输入在此模式下拒绝处理。
    /// 与 --pdf-force-ocr / --ofd-force-ocr 互斥（同时给出立即报错）。
    #[arg(long)]
    text_only: bool,
    /// OCR 推理线程数（页级并行）。A 改造后：进程级 ORT 线程池按
    /// `intra = max(1, 核心数/threads)` 提交，使总线程≈核心数、不再超额订阅。
    /// 默认 0 = 自动取可用并行度（飞腾 D2000 8 核→8），结合 intra=1 全核利用；
    /// 内存受限环境（cgroup<8GB）可显式调小。
    #[arg(long, default_value_t = 0)]
    threads: usize,
    /// 渲染 DPI（图片型 PDF/OFD 走 OCR 时的渲染分辨率）。越低像素越少、渲染与
    /// 文本检测(det)越快，但字号过小会漏检；印刷体公文 100 零精度损失且比 200
    /// 快 33%，80 起脚注/小字开始漏检。实测 上海公报52p: 100 vs 200 恢复率均 99.83%。
    /// 合法区间 50–400，越界或 NaN 立即报错（不跑半途）。
    #[arg(long, default_value_t = 100.0)]
    dpi: f32,
    /// 页码选择（仅 PDF）：1 基含端点，逗号分隔，如 "1-5,8"；rN 从末页倒数
    /// （"r3-r1" = 末三页），"all" = 全部（默认）。排序去重、越界裁剪；
    /// 与所选页无交集 / 倒序区间 / 非法语法立即报错。语法对齐 MinerU。
    #[arg(long)]
    pages: Option<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    // 审计 #9：--dpi 早期校验（NaN/Inf / 越界 [50,400] 立即退出）。库路径各通道
    // 内另有同语义闸（route_pdf / convert_ofd），这里只是 CLI 早失败、省掉
    // stdin 落盘等前置开销。
    if let Err(e) = anydoc_ocr::validate_render_dpi(cli.dpi) {
        exit_with_hint(&e);
    }
    // --pages 早期语法校验（同 dpi：非法语法早退，省掉 stdin 落盘等前置开销；
    // rN/空集需页数判定的在库侧 route_pdf 终审）。
    if let Err(e) = anydoc_ocr::validate_page_range_syntax(cli.pages.as_deref()) {
        exit_with_hint(&e);
    }
    // 默认档（mineru-basic）的模型预检**不在这里**做：CLI 无从判断该文档是否真的
    // 会走 OCR（文字型 PDF 一个模型都不加载），在下载发生前打印"正在下载"会变成
    // 对纯文字文档的假告警。真正的告知点在 `ocr_engine::OcrEngine::build`（只有
    // OCR 引擎要建模型时才输出一行）。
    let threads = if cli.threads == 0 {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
    } else {
        cli.threads
    };
    let opts = anydoc_ocr::ConvertRequest {
        render: anydoc_ocr::RenderConfig { dpi: cli.dpi },
        // 版面模型不再暴露为参数：默认档 = MinerU 的 PP-DocLayoutV2 全流程，
        // 换 table 版面会把它整个替掉、连 MinerU 对齐的后处理都失效，
        // 属"和默认流程作对"的旋钮，故从 CLI 撤下（库侧 OcrLayout 仍可显式构造）。
        ocr: anydoc_ocr::OcrConfig { tier: cli.ocr_tier, layout: OcrLayout::Doc },
        parallel: anydoc_ocr::ParallelConfig { page_parallel: threads, ort_intra: 0 },
        pages: cli.pages.clone(),
        // quality_route 已从 CLI 撤下（参数面取消，语义与 MinerU 默认档冲突，
        // 见 src/quality.rs）：恒用 Default = Off。库调用方仍可显式构造 Auto。
        ..Default::default()
    };
    let force = anydoc_ocr::ForceFlags {
        ofd_force_ocr: cli.ofd_force_ocr,
        pdf_force_ocr: cli.pdf_force_ocr,
        text_only: cli.text_only,
    };
    // #13：`--text-only` 与两个 force 开关互斥。CLI 早拒（不分格式）比"目录批处理
    // 里 PDF 报错、OFD 也报错、报错文案还不一样"好解释；库侧各通道另有同语义闸
    // （route_pdf / convert_ofd），库调用方不会被绕过。
    if cli.text_only && (cli.pdf_force_ocr || cli.ofd_force_ocr) {
        let mut v = Vec::new();
        if cli.pdf_force_ocr {
            v.push("--pdf-force-ocr");
        }
        if cli.ofd_force_ocr {
            v.push("--ofd-force-ocr");
        }
        exit_with_hint(&anydoc_ocr::ConvertError::new(
            anydoc_ocr::ErrorKind::Unsupported,
            anydoc_ocr::Stage::Convert,
            format!("--text-only 与 {} 互斥（前者绝不跑 OCR，后者强制跑）", v.join(" / ")),
        ));
    }

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
        // --pages 仅单文档合理（MinerU 同口径"Only works for single PDFs"）：
        // 目录批处理里逐文件套同一页集易生误解，直接早拒。
        if let Some(raw) = cli
            .pages
            .as_deref()
            .filter(|p| !p.trim().is_empty() && !p.eq_ignore_ascii_case("all"))
        {
            exit_with_hint(&anydoc_ocr::ConvertError::new(
                anydoc_ocr::ErrorKind::Unsupported,
                anydoc_ocr::Stage::Convert,
                format!("--pages {raw:?} 仅支持单文档输入，目录批处理不适用"),
            ));
        }
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
    opts: &anydoc_ocr::ConvertRequest,
    force: anydoc_ocr::ForceFlags,
    output: &Option<PathBuf>,
) -> Result<()> {
    let paths = anydoc_ocr::batch::collect_documents(input_dir);
    if paths.is_empty() {
        eprintln!("[batch] 目录 {} 下无受支持文档", input_dir.display());
        return Ok(());
    }
    let output_dir = output.as_ref().ok_or_else(|| {
        anydoc_ocr::ConvertError::new(
            anydoc_ocr::ErrorKind::Unsupported,
            anydoc_ocr::Stage::Output,
            "目录输入需要 --output 指定输出目录",
        )
    })?;
    std::fs::create_dir_all(output_dir)?;

    eprintln!(
        "[batch] 发现 {} 个文档，输出到 {}",
        paths.len(),
        output_dir.display()
    );
    let converter = anydoc_ocr::batch::BatchConverter::new(opts.clone(), force);
    // P1.10：convert_many 返回 Vec<DocOutcome>（path + 独立 Result），按输入顺序
    let outcomes = converter.convert_many(&paths);
    let mut ok = 0usize;
    let mut fail = 0usize;
    for (i, outcome) in outcomes.iter().enumerate() {
        let prefix = format!("[batch] ({}/{})", i + 1, outcomes.len());
        match &outcome.result {
            Ok(md) => {
                let out_path = output_dir.join(output_stem(input_dir, &outcome.path));
                if let Some(parent) = out_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&out_path, md)?;
                eprintln!("{prefix} {} → {}", outcome.path.display(), out_path.display());
                ok += 1;
            }
            Err(e) => {
                // ADR-0006 §7：按 e.code() 精准提示（ConvertError 直接有 code() 方法，
                // 无需 downcast）。code() 返稳定字符串，main 据此给"下一步建议"。
                let hint = error_hint(e);
                eprintln!("{prefix} {} 失败: {e}\n  提示: {hint}", outcome.path.display());
                fail += 1;
            }
        }
    }
    eprintln!("[batch] 完成：{ok} 成功，{fail} 失败");
    Ok(())
}

/// 按 `ConvertError::kind` 给用户精准提示（P1.9：提示语从 code() 字符串匹配
/// 迁移到 kind 枚举匹配——`Runtime` 拆出后不再与 `Malformed` 共用一条模糊文案）。
fn error_hint(e: &ConvertError) -> &'static str {
    use anydoc_ocr::ErrorKind;
    match e.kind {
        ErrorKind::Encrypted => "文档已加密，需提供密码或解密后重试",
        ErrorKind::Malformed => "文档损坏或结构不可用 — 检查文件完整性",
        ErrorKind::Runtime => {
            "运行时依赖失败（ORT/pdfium 未配置或推理出错）— 检查运行环境与模型文件"
        }
        ErrorKind::MissingPart => "文档结构不完整（缺必需部件），可能源文件生成不完整",
        ErrorKind::ResourceLimit => "超出安全限制（可能解压炸弹或文档过大）",
        ErrorKind::Unsupported => "格式不支持",
        ErrorKind::NeedsOcr => "存在无法从文字层恢复的页（扫描/乱码），按页补 OCR 未取回结果 — 可尝试 --pdf-force-ocr 整篇 OCR",
        ErrorKind::Io => "文件读写错误（路径不存在/权限不足/磁盘满）",
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
/// 审计 #8：不再 `read_to_end` 无界读——经 `read_stdin_bounded` 累计超过
/// `ANYDOC_MAX_INPUT_BYTES`（默认 200 MiB，对齐 MinerU 上传档）立即断读报
/// `ResourceLimit`，超限时内存不会被管道投喂打满。
fn resolve_stdin() -> Result<(PathBuf, Option<tempfile::NamedTempFile>)> {
    let buf = anydoc_ocr::read_stdin_bounded()?;
    let mut tmp = tempfile::NamedTempFile::new()?;
    Write::write_all(&mut tmp, &buf)?;
    let p = tmp.path().to_path_buf();
    Ok((p, Some(tmp)))
}
