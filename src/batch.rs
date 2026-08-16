//! 批处理入口（ADR-0005）：跨文档复用 OCR 引擎 + 跨文档流水线。
//!
//! `BatchConverter` 持 `ConvertOptions`，`convert_many` 内部对每个 path 做预分流：
//! - PDF：先试 `pdf::text_layer_markdown`（文字型快速路径，~0.03s/文档）；
//!   命中即出结果，未命中（图片型）收集到 `ocr_paths`
//! - 非 PDF（OFD/docx/xlsx/pptx）：委托 `convert_to_markdown`，per-doc 跑
//!   （ADR-0005：OFD 第一遍页型判定复杂，暂不接入跨文档 pipeline）
//!
//! 收集到的图片型 PDF paths 一次性送入 `pdf::convert_pdf_ocr`——跨文档 render↔OCR
//! pipeline，文档边界 OCR 池空转消除 + 小文档 setup 摊薄。engine 复用由
//! `OcrEngine::build` 的 `static CACHE` 自动命中（同进程内同 key 只建一次）。
//!
//! 错误隔离：单个文档失败不炸整批，`Vec<Result<String>>` 每文档独立 Result。
//! 跨文档 pipeline 整体失败 → 该批 ocr_paths 内每文档标 Err，其他文档不受影响。

use std::path::{Path, PathBuf};

use crate::convert_to_markdown;
use crate::detect::DocKind;
use crate::error::{Result, runtime};
use crate::{ConvertOptions, ForceFlags};

/// 批处理转换器：跨文档复用 OCR 引擎（ADR-0005）。
pub struct BatchConverter {
    opts: ConvertOptions,
    force: ForceFlags,
}

impl BatchConverter {
    pub fn new(opts: ConvertOptions, force: ForceFlags) -> Self {
        Self { opts, force }
    }

    /// 批量转换：每文档独立 Result（错误隔离），OCR 引擎跨文档复用。
    ///
    /// 预分流策略（ADR-0005 候选 2 + ADR-0006 错误分类）：
    /// 1. PDF：调 `text_layer_markdown`——`Ok(Some)` 出结果，`Ok(None)` 入 `ocr_paths`，
    ///    `Err`（Encrypted/Malformed）直接标错不送 OCR（ADR-0006 §5：加密/损坏 PDF
    ///    送 OCR 也读不了，且会丢失错误分类）
    /// 2. PDF + force_ocr：仍调 `text_layer_markdown` 做加密预检——`Ok`（含 None）
    ///    送 OCR，`Err` 直接标错（ADR-0006 §6：force_ocr 不绕过加密检查）
    /// 3. 非 PDF：委托 `convert_to_markdown`（OFD 内部仍 per-doc 流水线）
    /// 4. `ocr_paths` 一次性送入 `convert_pdf_ocr` 跨文档 pipeline
    pub fn convert_many(&self, paths: &[PathBuf]) -> Vec<Result<String>> {
        // 每文档槽位：None = 待填充，Some(r) = 已完成。
        // `ConvertError` 非 Clone，故用 `(0..n).map(|_| None).collect()` 避开 Clone 约束。
        let mut slots: Vec<Option<Result<String>>> = (0..paths.len()).map(|_| None).collect();

        // 1) PDF 预分流：文字型快速路径 + 收集图片型 paths + 加密/损坏 Err 直接标错
        let mut ocr_paths: Vec<(usize, PathBuf)> = Vec::new();
        for (i, path) in paths.iter().enumerate() {
            if crate::detect::detect(path) != DocKind::Pdf {
                continue;
            }
            match crate::pdf::text_layer_markdown(path, &self.opts) {
                Ok(Some(md)) => {
                    // 命中文字层：force_ocr 时忽略文字层结果送 OCR，否则出结果
                    if self.force.pdf_force_ocr {
                        ocr_paths.push((i, path.clone()));
                    } else {
                        slots[i] = Some(Ok(md));
                    }
                }
                Ok(None) => {
                    // 图片型 PDF：送 OCR pipeline
                    ocr_paths.push((i, path.clone()));
                }
                Err(e) => {
                    // ADR-0006 §5：加密/损坏 PDF 直接标错，不送 ocr_paths
                    // （加密 PDF 送 OCR 也读不了，损坏 PDF 浪费 OCR 资源，
                    //   且绕一大圈会丢失 Encrypted/Malformed 分类）。
                    // force_ocr 同样不绕过（ADR-0006 §6）。
                    slots[i] = Some(Err(e));
                }
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
                        slots[*orig_idx] = Some(Err(runtime(
                            None,
                            format!("跨文档 OCR 失败: {e}"),
                        )));
                    }
                }
            }
        }

        // 3) 非 PDF 文档（OFD/docx/xlsx/pptx）per-doc 转换
        for (i, path) in paths.iter().enumerate() {
            if slots[i].is_none() {
                slots[i] = Some(convert_to_markdown(path, &self.opts, self.force));
            }
        }

        // 4) 收集结果（所有槽位此时应已填充）
        slots
            .into_iter()
            .map(|s| s.expect("batch slot must be filled by step 3"))
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
    name.ends_with(".tmp")
        || name.ends_with(".crdownload")
        || name.starts_with("~$")
}
