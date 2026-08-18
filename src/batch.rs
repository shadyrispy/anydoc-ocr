//! 批处理入口（ADR-0005）：跨文档复用 OCR 引擎 + 跨文档流水线。
//!
//! P1.10 后 `BatchConverter` 内部走统一 convert 调度层
//! （[`crate::convert::route_doc`]，与单文档 [`crate::convert_to_markdown`]
//! 共用同一预分流）：文字层快速路径、加密/损坏预检、图片型 PDF 判定只此一处；
//! 跨文档 pipeline（`pdf::convert_pdf_ocr`）成为 convert 的实现细节。
//!
//! 收集到的图片型 PDF paths 一次性送入跨文档 render↔OCR pipeline——文档边界
//! OCR 池空转消除 + 小文档 setup 摊薄。engine 复用由 `OcrEngine::build` 的
//! `static CACHE` 自动命中（同进程内同 key 只建一次）。
//!
//! 错误隔离：单个文档失败不炸整批，[`DocOutcome`] 每文档独立 Result。
//! 跨文档 pipeline 整体失败 → 该批 ocr_paths 内每文档标 Err，其他文档不受影响。

use std::path::{Path, PathBuf};

use crate::convert::{DocRoute, ForceFlags, convert_per_doc, route_doc};
use crate::detect::DocKind;
use crate::error::{Result, Stage, runtime};
use crate::ConvertRequest;

/// 单文档转换结果（P1.10 固化返回结构）：输入路径 + 独立 Result，按输入顺序返回。
#[derive(Debug)]
pub struct DocOutcome {
    /// 输入文档路径（与 `convert_many` 入参一一对应，免去调用方 zip）
    pub path: PathBuf,
    /// 该文档的转换结果（错误隔离：单文档失败不影响其他文档）
    pub result: Result<String>,
}

/// 批处理转换器：跨文档复用 OCR 引擎（ADR-0005）。
pub struct BatchConverter {
    opts: ConvertRequest,
    force: ForceFlags,
}

impl BatchConverter {
    pub fn new(opts: ConvertRequest, force: ForceFlags) -> Self {
        Self { opts, force }
    }

    /// 批量转换：每文档独立 [`DocOutcome`]（错误隔离），OCR 引擎跨文档复用。
    ///
    /// 预分流走统一调度层 `route_doc`（与单文档入口共用，P1.10）：
    /// 1. PDF 文字层探针——`Done(Ok)` 出结果，`Done(Err)`（加密/损坏）直接标错
    ///    不送 OCR（ADR-0006 §5/§6），`Ocr`（图片型/force）收集到 `ocr_paths`
    /// 2. `ocr_paths` 一次性送入 `convert_pdf_ocr` 跨文档 pipeline
    /// 3. 非 PDF（OFD/docx/xlsx/pptx）走 `convert_per_doc` per-doc 转换
    pub fn convert_many(&self, paths: &[PathBuf]) -> Vec<DocOutcome> {
        // 每文档槽位：None = 待填充，Some(r) = 已完成。
        // `ConvertError` 非 Clone，故用 `(0..n).map(|_| None).collect()` 避开 Clone 约束。
        let mut slots: Vec<Option<Result<String>>> = (0..paths.len()).map(|_| None).collect();

        // 1) 统一调度预分流：文字型快速路径 + 加密/损坏标错 + 图片型收集
        let mut ocr_paths: Vec<(usize, PathBuf)> = Vec::new();
        let mut perdoc: Vec<(usize, DocKind)> = Vec::new();
        for (i, path) in paths.iter().enumerate() {
            match route_doc(path, &self.opts, &self.force) {
                DocRoute::Done(r) => slots[i] = Some(r),
                DocRoute::Ocr => ocr_paths.push((i, path.clone())),
                DocRoute::PerDoc(kind) => perdoc.push((i, kind)),
            }
        }

        // 2) 跨文档 OCR pipeline（图片型 PDF 集中处理）
        if !ocr_paths.is_empty() {
            let just_paths: Vec<PathBuf> = ocr_paths.iter().map(|(_, p)| p.clone()).collect();
            match crate::pdf::convert_pdf_ocr(&just_paths, &self.opts) {
                Ok(md_per_doc) => {
                    // convert_pdf_ocr 返回 Vec<(doc_idx, Result<String>)> 按 doc_idx
                    // 升序；每文档独立 Result。doc_idx 与 just_paths 索引一一对应，
                    // 回填到原始 paths 槽位。Err doc 带真实 detail（ADR 候选 3）——
                    // 不再需要"槽位缺失→猜 Err(详见 stderr)"的兜底。
                    for (doc_idx, md) in md_per_doc {
                        let (orig_idx, _) = &ocr_paths[doc_idx];
                        slots[*orig_idx] = Some(md);
                    }
                }
                Err(e) => {
                    // pipeline 整体失败（绑定/ORT 致命错误）→ 该批 ocr_paths 全标 Err
                    for (orig_idx, _) in &ocr_paths {
                        slots[*orig_idx] =
                            Some(Err(runtime(Stage::Ocr, None, format!("跨文档 OCR 失败: {e}"))));
                    }
                }
            }
        }

        // 3) 非 PDF 文档（OFD/docx/xlsx/pptx）per-doc 转换
        for (i, kind) in perdoc {
            slots[i] = Some(convert_per_doc(&paths[i], kind, &self.opts, &self.force));
        }

        // 4) 收集结果（所有槽位此时应已填充；防御式兜底而非 panic——P0-2）
        paths
            .iter()
            .zip(slots)
            .map(|(path, s)| DocOutcome {
                path: path.clone(),
                result: s.unwrap_or_else(|| {
                    Err(runtime(Stage::Convert, None, "内部错误：批处理槽位未填充"))
                }),
            })
            .collect()
    }
}

/// 递归遍历目录，收集受支持的文档文件（PDF/OFD/办公格式）。
///
/// 跳过隐藏文件（`.` 开头）、临时文件（`.tmp`/`.crdownload`/`~$` Office 锁文件）
/// 和符号链接（防环）。结果按路径排序，保证批处理顺序确定。
pub fn collect_documents(input: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_dir(input, &mut out);
    out.sort();
    out
}

fn collect_dir(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        eprintln!("[batch] 警告：无法读取目录 {}", dir.display());
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.starts_with('.') {
            continue;
        }
        if path.is_symlink() {
            continue;
        }
        if path.is_dir() {
            collect_dir(&path, out);
        } else if is_supported_doc(&path) {
            out.push(path);
        }
    }
}

/// 受支持文档扩展名（大小写不敏感）。PDF/OFD 走本库专用通道，其余走 anydoc。
fn is_supported_doc(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    let ext = ext.to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "pdf" | "ofd" | "docx" | "doc" | "xlsx" | "xls" | "pptx" | "ppt"
    ) && !is_temp_file(path)
}

fn is_temp_file(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    name.ends_with(".tmp") || name.ends_with(".crdownload") || name.starts_with("~$")
}
