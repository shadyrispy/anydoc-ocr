//! `--indra` 对比通道：走 `oar_ocr::indra`（上游 0.10.1+ 原生 ONNX pipeline）
//! 输出其自带 Markdown，绕过本仓 StructureResult → GFM 组装链。
//!
//! 定位是**引擎对比工具**，不是主转换通路：不做文字层回退、不做质量路由、
//! 不做表格后处理（indra 自带 HTML 表格）。模型不走 auto-download 注册表，
//! 需自行准备 datalab-to/indra-ocr 权重（OpenRAIL-M 许可，见
//! `indra-vs-ppocrv6-bench.md`），目录解析顺序：
//! `ANYDOC_INDRA_DIR` > `$ANYDOC_MODEL_DIR/indra` > `./models/datalab-to/indra-ocr`。

use std::path::{Path, PathBuf};

use crate::error::{ConvertError, ErrorKind, Result, Stage};

/// 解析 indra 模型目录：显式 env > `ANYDOC_MODEL_DIR/indra` > cwd 相对路径。
/// 返回 (目录, 来源说明)，目录不存在时返回 Err 带下载指引。
pub fn resolve_indra_model_dir() -> std::result::Result<(PathBuf, String), String> {
    let candidates: Vec<(PathBuf, String)> = [
        std::env::var("ANYDOC_INDRA_DIR").ok().map(|d| {
            (PathBuf::from(d), "ANYDOC_INDRA_DIR".to_string())
        }),
        std::env::var("ANYDOC_MODEL_DIR")
            .ok()
            .map(|d| (PathBuf::from(d).join("indra"), "$ANYDOC_MODEL_DIR/indra".to_string())),
        Some((
            PathBuf::from("models/datalab-to/indra-ocr"),
            "./models/datalab-to/indra-ocr（cwd 相对）".to_string(),
        )),
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("models/datalab-to/indra-ocr")))
            .map(|d| (d, "<exe目录>/models/datalab-to/indra-ocr".to_string())),
    ]
    .into_iter()
    .flatten()
    .collect();
    for (dir, src) in &candidates {
        // 必须含 det/rec 模型才算有效目录（防"目录在但空的"半配置）
        if dir.join("det.onnx").exists() || dir.join("det.onnx_data").exists()
            || dir.read_dir().map(|mut it| it.next().is_some()).unwrap_or(false)
        {
            return Ok((dir.clone(), src.clone()));
        }
    }
    Err(format!(
        "未找到 indra 模型（依次尝试: {}）。\n\
         模型不走 auto-download，需自行从 HuggingFace 下载 datalab-to/indra-ocr \
         （国内可用 HF_ENDPOINT=https://hf-mirror.com）：\n  \
         huggingface-cli download datalab-to/indra-ocr --local-dir <dir>\n\
         然后设 ANYDOC_INDRA_DIR=<dir> 或放到 ./models/datalab-to/indra-ocr",
        candidates
            .iter()
            .map(|(_, s)| s.as_str())
            .collect::<Vec<_>>()
            .join(" > ")
    ))
}

/// indra 对比通道入口：单文档（PDF 或图片）→ 每页 Markdown 拼接。
pub fn convert_indra_to_markdown(input: &Path, dpi: f32) -> Result<String> {
    let (model_dir, src) = resolve_indra_model_dir().map_err(|msg| {
        ConvertError::new(ErrorKind::MissingPart, Stage::Convert, msg)
    })?;
    eprintln!("[indra] 模型目录: {}（{}）", model_dir.display(), src);

    let images: Vec<image::RgbImage> = if input
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("pdf"))
        .unwrap_or(false)
    {
        crate::pdf::render::render_pdf_pages(input, dpi, &[])?
    } else {
        // 单图输入（png/jpg 等走 image crate 直读）
        let img = image::open(input).map_err(|e| {
            ConvertError::new(
                ErrorKind::Unsupported,
                Stage::Render,
                format!("--indra 仅支持 PDF 或单张图片输入，读取失败 {}: {e}", input.display()),
            )
        })?;
        vec![img.into_rgb8()]
    };
    eprintln!("[indra] {} 页，开始识别…", images.len());

    let ocr = oar_ocr::indra::IndraOcrBuilder::new(&model_dir).build().map_err(|e| {
        ConvertError::new(
            ErrorKind::Runtime,
            Stage::Ocr,
            format!("IndraOcr 构建失败（目录 {}）: {e}", model_dir.display()),
        )
    })?;
    let pages = ocr.predict(images).map_err(|e| {
        ConvertError::new(ErrorKind::Runtime, Stage::Ocr, format!("indra 推理失败: {e}"))
    })?;

    // 页间以水平分隔线衔接（GFM 惯例）；页内 markdown 即 indra 原生输出
    let md = pages
        .iter()
        .map(|p| p.markdown.trim().to_string())
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");
    Ok(if md.is_empty() { String::new() } else { md + "\n" })
}
